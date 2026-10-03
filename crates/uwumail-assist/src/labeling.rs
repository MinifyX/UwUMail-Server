//! Labels with the assistant (docs/labels.md, "How a label is chosen"): the cheap ways first (the
//! label's rules, its detector, learned senders, similar mails, the classifier), the model only for
//! the labels they leave in doubt, held to the facts read from the mail, and at most a main label
//! and a second one.

use std::collections::HashMap;

use uwumail_labels::similar::{self, Neighbour};
use uwumail_labels::{
    AiVerdict, Base, Decision, Detector, Facts, Label, Likeness, MAIN_THRESHOLD, Mail, Rules, ai_candidates, ask_about,
    choose, merge,
};
use uwumail_store::{Account, AssistLabel, EmailRecord, KeywordsChange, LabelLogWrite};

use crate::access::Embedder;
use crate::features::TYPICAL_LABEL_TOKENS_PER_LABEL;
use crate::features::{LabelPick, authentication, parse_labels};
use crate::llm;
use crate::mail::MailText;
use crate::prompts::{self, MAX_PROMPT_SHOTS, PromptLabel, PromptShot};
use crate::{Assist, AssistError, Result};

/// Characters of a mail the model reads for its labels.
const LABEL_MAIL_CHARS: usize = 4000;
/// Characters of a mail an embedding is made of.
const EMBED_TEXT_CHARS: usize = 2000;
/// Bytes of a message parsed for the facts, at most (as at delivery).
const MAX_PARSE_BYTES: usize = 4 * 1024 * 1024;
/// Labeled mails without an embedding that get one along with each new mail.
const BACKFILL_PER_MAIL: usize = 8;
/// A cheap way's finding this sure is shown to the model as a hint.
const HINT_MIN: f64 = 0.3;
/// With labels without a model switched off, the cheap ways only give hints: no surer than this.
const HINT_ONLY: f64 = 0.79;

/// The labels as [`uwumail_labels`] sees them, borrowing from the stored ones.
fn label_views<'a>(stored: &'a [AssistLabel], rules: &'a [Option<Rules>]) -> Vec<Label<'a>> {
    stored
        .iter()
        .zip(rules)
        .map(|(label, rules)| Label {
            id: label.id,
            keyword: &label.keyword,
            rules: rules.as_ref(),
            detector: label.detector.as_deref().and_then(Detector::parse),
            learn_senders: label.learn_senders,
            classifier: label.classifier,
            base: label.base.as_deref().and_then(Base::parse),
            auto: label.auto,
        })
        .collect()
}

/// A label for the prompt: a base label with its definition and examples in the language they were
/// set up in, one of the person's own with their description.
fn prompt_label(label: &AssistLabel) -> PromptLabel {
    match label.base.as_deref().and_then(Base::parse) {
        Some(base) => {
            let language = base_label_language(base, label);
            let text = base.text(language);
            // What the person had written for the label before it became a base label: a hint that
            // says what they mean by it, the definition still decides (security review LABELS22-L2).
            let description = match label.previous_description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                Some(own) => {
                    let own: String = own.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(500).collect();
                    format!("{} The person's own words for this label (a hint): \"{own}\"", text.description)
                }
                None => text.description.to_owned(),
            };
            PromptLabel {
                name: label.name.clone(),
                description,
                examples: text.examples.iter().map(|e| e.to_string()).collect(),
                counter_examples: text.counter_examples.iter().map(|e| e.to_string()).collect(),
            }
        }
        None => PromptLabel { name: label.name.clone(), description: label.description.clone(), ..Default::default() },
    }
}

/// The language a base label was set up in: its definition was written in the person's language
/// when it was made (and may be an older wording since), its name too.
fn base_label_language(base: Base, label: &AssistLabel) -> &'static str {
    let description = label.description.as_str();
    if description == base.text("en").description || label.name.trim().eq_ignore_ascii_case(base.name("en")) {
        "en"
    } else if description == base.text("de").description
        || label.name.trim().eq_ignore_ascii_case(base.name("de"))
        || [" du ", " dein", " dich ", " und "].iter().any(|word| description.contains(word))
    {
        "de"
    } else {
        "en"
    }
}

/// What an embedding is made of: subject, sender domain and the start of the text. nomic-embed-text
/// wants to be told what the embedding is for.
fn embed_text(model: &str, mail: &MailText) -> String {
    let domain = mail.from.first().and_then(|a| a.email.rsplit_once('@')).map(|(_, d)| d).unwrap_or_default();
    let text: String = format!("{}\n{}\n{}", mail.subject, domain, mail.text).chars().take(EMBED_TEXT_CHARS).collect();
    if model.to_ascii_lowercase().contains("nomic") { format!("classification: {text}") } else { text }
}

impl Assist {
    /// Puts labels on one of the person's mails: what the cheap ways are sure of, and what the model
    /// confirms of the labels they leave in doubt. Labels on the mail count against the two it may
    /// have; none is ever taken off. Without a model, or when it fails after the cheap ways were
    /// sure of something, only their labels go on.
    pub async fn label_email(&self, account: &Account, email_id: i64) -> Result<Vec<LabelPick>> {
        let store = self.store();
        let record = self.record_of(account, email_id).await?;
        store.ensure_base_labels(account.id, "en").await?;
        let stored = store.assist_labels(account.id).await?;
        if stored.iter().all(|label| !label.auto) {
            return Ok(Vec::new());
        }
        let present: Vec<String> = record.keywords.iter().map(|k| k.to_lowercase()).collect();
        let raw = store.blob(&record.blob).await?;
        let mail_text = MailText::read(&record, &raw, LABEL_MAIL_CHARS);
        let cut = raw[..raw.len().min(MAX_PARSE_BYTES)].to_vec();
        let (mut mail, tokens) = tokio::task::spawn_blocking(move || {
            let mail = Mail::parse(&cut);
            let tokens: Vec<i64> =
                uwumail_labels::tokens(&mail).iter().map(|token| uwumail_labels::token_hash(token)).collect();
            (mail, tokens)
        })
        .await
        .map_err(|err| AssistError::Store(uwumail_store::StoreError::Internal(err.to_string())))?;
        if !mail.from.is_empty() && store.account_owns_address(account.id, &mail.from).await? {
            return Ok(Vec::new());
        }
        let auth = authentication(&mail_text.headers, Some(self.hostname()), &record.from);
        let pass = |result: &Option<String>| result.as_deref() == Some("pass");
        mail.from_trusted = pass(&auth.dmarc) || (pass(&auth.dkim) && pass(&auth.spf));
        // Known only when authentication backs the From address (security review 0.22 LABELS22-L1).
        mail.known_sender =
            mail.from_trusted && !mail.from.is_empty() && store.knows_sender(account.id, mail.from.clone()).await?;
        let facts = Facts::of(&mail);

        let rules: Vec<Option<Rules>> =
            stored.iter().map(|label| label.rules.as_ref().and_then(|r| Rules::check(r).ok().flatten())).collect();
        let labels = label_views(&stored, &rules);
        let classifiers = stored.iter().filter(|label| label.classifier).map(|label| label.id).collect();
        let mut knowledge = store.label_knowledge(account.id, mail.from.clone(), tokens.clone(), classifiers).await?;
        knowledge.similar = self.similar(account, email_id, &mail_text, &tokens).await?;
        let mut candidates = uwumail_labels::candidates(&labels, &mail, &present, &knowledge, &tokens);
        let prefs = store.assist_prefs(account.id).await?;
        if !prefs.non_ai_labels {
            for candidate in &mut candidates {
                candidate.confidence = candidate.confidence.min(HINT_ONLY);
            }
        }
        let sure = choose(&labels, &present, candidates.clone());
        let mut asked = ask_about(&labels, &present, &candidates);
        // A label the person took off this sender's mail by hand is not asked about either.
        if !mail.from.is_empty() {
            asked.retain(|id| knowledge.senders.get(id).is_none_or(|count| *count >= 0));
        }
        let mut from_model = Vec::new();
        let mut effective = None;
        if !asked.is_empty() {
            let asked_labels: Vec<&AssistLabel> = stored.iter().filter(|l| asked.contains(&l.id)).collect();
            match self.ask_labels(account, &asked_labels, &mail_text, &facts, &candidates, prefs.auto_labels).await {
                Ok((verdicts, used)) => {
                    from_model = ai_candidates(&labels, &facts, &asked, &verdicts, &candidates);
                    effective = Some(used);
                }
                Err(AssistError::Unavailable(_)) => {}
                Err(err) if sure.is_empty() => return Err(err),
                Err(err) => {
                    tracing::info!(account = account.id, %err, "the model gave no labels, keeping the sure ones");
                }
            }
        }
        let chosen = choose(&labels, &present, merge(candidates, from_model));
        self.put_labels(account, email_id, &stored, chosen, effective).await
    }

    async fn record_of(&self, account: &Account, email_id: i64) -> Result<EmailRecord> {
        match self.store().email(account.id, email_id).await {
            Ok(record) => Ok(record),
            Err(uwumail_store::StoreError::NotFound(_)) => Err(AssistError::NotFound(format!("email {email_id}"))),
            Err(err) => Err(err.into()),
        }
    }

    /// What the model says of `asked`, with the facts, the hints of the cheap ways and the person's
    /// corrections of these labels.
    async fn ask_labels(
        &self,
        account: &Account,
        asked: &[&AssistLabel],
        mail: &MailText,
        facts: &Facts,
        candidates: &[Decision],
        with_shots: bool,
    ) -> Result<(Vec<AiVerdict>, crate::Effective)> {
        let ticket =
            self.prepare(account, "autoLabels").await?.expecting(TYPICAL_LABEL_TOKENS_PER_LABEL * asked.len() as i64);
        let list: Vec<PromptLabel> = asked.iter().map(|label| prompt_label(label)).collect();
        let name_of: HashMap<i64, &str> = asked.iter().map(|label| (label.id, label.name.as_str())).collect();
        let hints: Vec<(String, String)> = candidates
            .iter()
            .filter(|candidate| candidate.confidence >= HINT_MIN)
            .filter_map(|candidate| {
                let name = name_of.get(&candidate.label_id)?;
                let sure = if candidate.confidence >= MAIN_THRESHOLD { "" } else { " (not sure)" };
                Some((name.to_string(), format!("{}{sure}", candidate.reason)))
            })
            .collect();
        // The person's corrections only while they have AI labels on (security review LABELS22-L3).
        let stored_shots = if with_shots { self.store().label_shots(account.id).await? } else { Vec::new() };
        let shots: Vec<PromptShot> = stored_shots
            .into_iter()
            .filter_map(|shot| {
                Some(PromptShot {
                    label: name_of.get(&shot.label_id)?.to_string(),
                    positive: shot.positive,
                    sender_domain: shot.sender_domain,
                    subject: shot.subject,
                    snippet: shot.snippet,
                })
            })
            .take(MAX_PROMPT_SHOTS)
            .collect();
        let prompt = prompts::labels(mail, &list, &facts.for_prompt(), &hints, &shots);
        let (completion, effective) = self.send(ticket, &prompt, None).await?;
        let answer = llm::json_answer(&completion.text).ok_or_else(|| AssistError::ProviderFailed {
            description: "the model's answer was not a list of labels".into(),
            retry_after: None,
            transient: false,
        })?;
        let owned: Vec<AssistLabel> = asked.iter().map(|label| (*label).clone()).collect();
        Ok((parse_labels(&answer, &owned), effective))
    }

    /// The server's embeddings provider, when mail may go to it for labels: AI labels are on on the
    /// server and for the person, the person may use the assistant for them, and the model they
    /// chose for labels is one of the server's. Someone who picked a personal or local model, or
    /// switched AI labels off, keeps their mail to themselves (security review 0.22 LABELS22-M1).
    pub(crate) async fn label_embedder(&self, account: &Account) -> Result<Option<Embedder>> {
        if !self.store().assist_prefs(account.id).await?.auto_labels {
            return Ok(None);
        }
        match self.resolve_for(account, "autoLabels", false).await {
            Ok((provider, _, _)) if provider.server => self.embedder(account).await,
            Ok(_) | Err(AssistError::Unavailable(_)) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// How like the person's labeled mails this one is: by embeddings when the server has an
    /// embeddings provider the mail may go to ([`Assist::label_embedder`]), by tokens otherwise or
    /// when it fails.
    async fn similar(
        &self,
        account: &Account,
        email_id: i64,
        mail: &MailText,
        tokens: &[i64],
    ) -> Result<HashMap<i64, Likeness>> {
        if let Some(embedder) = self.label_embedder(account).await? {
            match self.similar_by_embeddings(account, &embedder, email_id, mail).await {
                Ok(found) => return Ok(found),
                Err(err) => tracing::info!(account = account.id, %err, "no embeddings, comparing tokens instead"),
            }
        }
        let examples = self.store().label_token_sets(account.id).await?;
        let mine = similar::token_set(tokens);
        // Thousands of examples of hundreds of tokens each: plain computing, off the async runtime
        // (security review 0.22 LABELS22-L4).
        tokio::task::spawn_blocking(move || {
            let neighbours = examples
                .into_iter()
                .filter(|example| example.email_id != email_id)
                .map(|example| Neighbour {
                    similarity: similar::jaccard_of_sets(&mine, &similar::token_set(&example.tokens)),
                    labels: example.labels,
                })
                .collect();
            similar::vote(neighbours, similar::TOKENS)
        })
        .await
        .map_err(|err| AssistError::Store(uwumail_store::StoreError::Internal(err.to_string())))
    }

    /// Embeds the mail, and along with it up to [`BACKFILL_PER_MAIL`] labeled mails that have no
    /// embedding yet, and lets the most alike vote.
    async fn similar_by_embeddings(
        &self,
        account: &Account,
        embedder: &Embedder,
        email_id: i64,
        mail: &MailText,
    ) -> Result<HashMap<i64, Likeness>> {
        let store = self.store();
        let model = embedder.model.clone();
        let mut ids = vec![email_id];
        let mut texts = vec![embed_text(&model, mail)];
        for id in store.label_examples_without_vector(account.id, model.clone(), BACKFILL_PER_MAIL).await? {
            if id == email_id {
                continue;
            }
            let Ok(record) = store.email(account.id, id).await else { continue };
            let Ok(raw) = store.blob(&record.blob).await else { continue };
            ids.push(id);
            texts.push(embed_text(&model, &MailText::read(&record, &raw, EMBED_TEXT_CHARS)));
        }
        let vectors = self.embed(account, embedder, "autoLabels", &texts).await?;
        let mut this = None;
        for (id, vector) in ids.iter().zip(&vectors) {
            let Some(stored) = similar::quantize(vector) else { continue };
            if *id == email_id {
                this = Some(stored.clone());
            }
            // The mail itself is kept too when it is a labeled one.
            store.set_label_vector(account.id, *id, model.clone(), stored).await?;
        }
        let Some(this) = this else { return Ok(HashMap::new()) };
        let examples = store.label_vectors(account.id, model).await?;
        tokio::task::spawn_blocking(move || {
            let neighbours = examples
                .into_iter()
                .filter(|example| example.email_id != email_id)
                .filter_map(|example| {
                    Some(Neighbour { similarity: similar::cosine(&this, &example.vector)?, labels: example.labels })
                })
                .collect();
            similar::vote(neighbours, similar::EMBEDDINGS)
        })
        .await
        .map_err(|err| AssistError::Store(uwumail_store::StoreError::Internal(err.to_string())))
    }

    /// Puts the chosen labels on and logs where each came from.
    async fn put_labels(
        &self,
        account: &Account,
        email_id: i64,
        stored: &[AssistLabel],
        chosen: Vec<Decision>,
        effective: Option<crate::Effective>,
    ) -> Result<Vec<LabelPick>> {
        if chosen.is_empty() {
            return Ok(Vec::new());
        }
        let change = KeywordsChange::Patch(chosen.iter().map(|d| (d.keyword.clone(), true)).collect());
        let update = uwumail_store::EmailUpdate { id: email_id, keywords: change, ..Default::default() };
        if let Some(Err(err)) = self.store().update_emails_by_server(account.id, vec![update]).await?.pop() {
            return Err(err.into());
        }
        let (provider, model) = effective.map(|e| (e.provider_name.clone(), e.model.clone())).unwrap_or_default();
        let entries = chosen
            .iter()
            .map(|decision| {
                let ai = decision.source == uwumail_labels::Source::Ai;
                let mut params = decision.params.clone();
                if let Some(object) = params.as_object_mut() {
                    object
                        .insert("confidence".into(), serde_json::json!((decision.confidence * 100.0).round() / 100.0));
                }
                LabelLogWrite {
                    email_id,
                    label_id: decision.label_id,
                    source: decision.source.as_str().to_owned(),
                    code: decision.code.to_owned(),
                    params,
                    reason: decision.reason.clone(),
                    provider: if ai { provider.clone() } else { String::new() },
                    model: if ai { model.clone() } else { String::new() },
                }
            })
            .collect();
        self.store().add_label_log_entries(account.id, entries).await?;
        Ok(chosen
            .into_iter()
            .filter_map(|decision| {
                let label = stored.iter().find(|label| label.id == decision.label_id)?.clone();
                Some(LabelPick {
                    label,
                    reason: decision.reason,
                    source: decision.source.as_str(),
                    confidence: decision.confidence,
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Security review 0.22 LABELS22-L2: what the person had written for an adopted label goes to
    /// the model as a hint next to the base definition.
    #[test]
    fn an_adopted_label_brings_the_persons_words_as_a_hint() {
        let label = AssistLabel {
            id: 1,
            name: "Rechnung".into(),
            description: Base::Invoice.text("de").description.into(),
            keyword: "rechnung".into(),
            color: None,
            created_at: 0,
            rules: None,
            detector: None,
            learn_senders: true,
            classifier: true,
            base: Some("invoice".into()),
            auto: true,
            previous_description: Some("Alles vom\nSteuerberater".into()),
        };
        let prompt = prompt_label(&label);
        assert!(prompt.description.starts_with(Base::Invoice.text("de").description));
        assert!(prompt.description.ends_with("(a hint): \"Alles vom Steuerberater\""), "{}", prompt.description);
        let plain = prompt_label(&AssistLabel { previous_description: None, ..label });
        assert_eq!(plain.description, Base::Invoice.text("de").description);
    }
}
