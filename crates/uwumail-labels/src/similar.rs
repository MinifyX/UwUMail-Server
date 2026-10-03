//! Nearest neighbours (docs/labels.md, "Similar mails"): a new mail is compared with the person's
//! labeled mails, by embedding vectors when the server has an embeddings provider, by their tokens
//! otherwise, and the labels of the most alike vote.
//!
//! Vectors are kept small: normalized to length 1, then one signed byte per dimension and one scale
//! for all, so 768 dimensions take 772 bytes.

use std::collections::HashMap;

use crate::Likeness;

/// Neighbours that vote.
pub const K: usize = 7;
/// Dimensions of a vector, at most.
pub const MAX_DIMENSIONS: usize = 4096;

/// How alike two mails must be to count, and to be sure.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Less alike neighbours are left out.
    pub min: f64,
    /// The best neighbour with a label must be this alike for the label to be sure.
    pub strong: f64,
}

/// For cosine similarity of embeddings (measured with nomic-embed-text and OpenAI's small model).
pub const EMBEDDINGS: Thresholds = Thresholds { min: 0.55, strong: 0.78 };
/// For the Jaccard similarity of token sets.
pub const TOKENS: Thresholds = Thresholds { min: 0.12, strong: 0.35 };

/// A labeled mail and how alike it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    /// The labels it has (none: an ordinary mail without labels).
    pub labels: Vec<i64>,
    pub similarity: f64,
}

/// A vector as it is stored: the scale as a little-endian `f32`, then one `i8` per dimension.
/// `None` for an empty, too long or not finite vector.
pub fn quantize(vector: &[f32]) -> Option<Vec<u8>> {
    if vector.is_empty() || vector.len() > MAX_DIMENSIONS || vector.iter().any(|x| !x.is_finite()) {
        return None;
    }
    let length = vector.iter().map(|x| f64::from(*x) * f64::from(*x)).sum::<f64>().sqrt();
    if length == 0.0 {
        return None;
    }
    let max = vector.iter().map(|x| (f64::from(*x) / length).abs()).fold(0.0, f64::max);
    let scale = (max / 127.0) as f32;
    let mut out = Vec::with_capacity(4 + vector.len());
    out.extend_from_slice(&scale.to_le_bytes());
    for x in vector {
        let q = (f64::from(*x) / length / f64::from(scale)).round().clamp(-127.0, 127.0) as i8;
        out.push(q as u8);
    }
    Some(out)
}

/// Dimensions of a stored vector.
pub fn dimensions(stored: &[u8]) -> usize {
    stored.len().saturating_sub(4)
}

/// The cosine similarity of two stored vectors; `None` when they do not fit together.
pub fn cosine(a: &[u8], b: &[u8]) -> Option<f64> {
    if a.len() != b.len() || a.len() <= 4 {
        return None;
    }
    let (mut dot, mut aa, mut bb) = (0i64, 0i64, 0i64);
    for (x, y) in a[4..].iter().zip(&b[4..]) {
        let (x, y) = (i64::from(*x as i8), i64::from(*y as i8));
        dot += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa == 0 || bb == 0 {
        return None;
    }
    Some(dot as f64 / ((aa as f64).sqrt() * (bb as f64).sqrt()))
}

/// The Jaccard similarity of two token lists (each token counted once).
pub fn jaccard(a: &[i64], b: &[i64]) -> f64 {
    jaccard_of_sets(&token_set(a), &token_set(b))
}

/// Tokens sorted and without repeats, as [`jaccard_of_sets`] takes them: a mail compared with many
/// examples is made a set once.
pub fn token_set(tokens: &[i64]) -> Vec<i64> {
    let mut set = tokens.to_vec();
    set.sort_unstable();
    set.dedup();
    set
}

/// [`jaccard`] of two [`token_set`]s.
pub fn jaccard_of_sets(a: &[i64], b: &[i64]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut both) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both += 1;
                i += 1;
                j += 1;
            }
        }
    }
    both as f64 / (a.len() + b.len() - both) as f64
}

/// What the [`K`] most alike `neighbours` say of each label they have. A label is sure (0.8 to
/// 0.95) when at least 3 of them have it, they hold at least 60 % of the votes (weighted by
/// similarity) and the best of them is at least `thresholds.strong` alike; otherwise its confidence
/// stays below [`crate::MAIN_THRESHOLD`], a hint for the model.
pub fn vote(mut neighbours: Vec<Neighbour>, thresholds: Thresholds) -> HashMap<i64, Likeness> {
    neighbours.retain(|n| n.similarity.is_finite() && n.similarity >= thresholds.min);
    neighbours.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
    neighbours.truncate(K);
    let total: f64 = neighbours.iter().map(|n| n.similarity).sum();
    let mut out: HashMap<i64, Likeness> = HashMap::new();
    if total <= 0.0 {
        return out;
    }
    let mut votes: HashMap<i64, (f64, usize, f64)> = HashMap::new();
    for neighbour in &neighbours {
        for label in &neighbour.labels {
            let entry = votes.entry(*label).or_insert((0.0, 0, 0.0));
            entry.0 += neighbour.similarity;
            entry.1 += 1;
            entry.2 = entry.2.max(neighbour.similarity);
        }
    }
    for (label, (weight, count, best)) in votes {
        let share = weight / total;
        let confidence = if count >= 3 && share >= 0.6 && best >= thresholds.strong {
            0.8 + 0.15 * ((share - 0.6) / 0.4).min(1.0)
        } else {
            (share * 0.75).min(0.75)
        };
        out.insert(label, Likeness { confidence, neighbours: count, similarity: best });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mail_made_a_set_once_compares_the_same() {
        let mail = [5, 3, 3, 9, 1];
        let example = [9, 9, 2, 3];
        let mine = token_set(&mail);
        assert_eq!(mine, [1, 3, 5, 9]);
        assert_eq!(jaccard_of_sets(&mine, &token_set(&example)), jaccard(&mail, &example));
        assert_eq!(jaccard(&mail, &example), 2.0 / 5.0);
        assert_eq!(jaccard_of_sets(&[], &mine), 0.0);
    }

    #[test]
    fn vectors_round_trip() {
        let a = quantize(&[0.3, -0.2, 0.9, 0.0]).unwrap();
        assert_eq!(dimensions(&a), 4);
        assert!((cosine(&a, &a).unwrap() - 1.0).abs() < 1e-9);
        let b = quantize(&[-0.3, 0.2, -0.9, 0.0]).unwrap();
        assert!((cosine(&a, &b).unwrap() + 1.0).abs() < 1e-9);
        assert_eq!(cosine(&a, &quantize(&[1.0, 2.0]).unwrap()), None);
        assert_eq!(quantize(&[0.0, 0.0]), None);
        assert_eq!(quantize(&[f32::NAN]), None);
    }

    #[test]
    fn voting() {
        assert!((jaccard(&[1, 2, 3], &[2, 3, 4]) - 0.5).abs() < 1e-9);
        let n = |labels: &[i64], similarity: f64| Neighbour { labels: labels.to_vec(), similarity };
        let votes = vote(vec![n(&[1], 0.9), n(&[1], 0.85), n(&[1], 0.8), n(&[], 0.6), n(&[2], 0.1)], EMBEDDINGS);
        assert!(votes[&1].confidence >= 0.8, "{votes:?}");
        assert!(!votes.contains_key(&2));
        let doubt = vote(vec![n(&[1], 0.9), n(&[3], 0.85), n(&[], 0.8)], EMBEDDINGS);
        assert!(doubt[&1].confidence < 0.8);
    }
}
