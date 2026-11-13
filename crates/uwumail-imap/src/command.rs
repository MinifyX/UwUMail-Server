//! IMAP commands as the parser hands them to a session.

/// A number in a sequence set: a value or `*`, the largest number in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqNum {
    Value(u32),
    Largest,
    /// `$`: the result a `SEARCH RETURN (SAVE)` kept (RFC 5182). The session puts the saved
    /// messages in its place before using the set; left alone it matches nothing.
    Saved,
}

impl SeqNum {
    fn resolve(self, largest: u32) -> u32 {
        match self {
            SeqNum::Value(value) => value,
            SeqNum::Largest => largest,
            SeqNum::Saved => 0,
        }
    }
}

/// Message sequence numbers or UIDs, like `1:4,7,9:*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceSet(pub Vec<(SeqNum, SeqNum)>);

impl SequenceSet {
    /// Whether `number` is in the set, with `*` standing for `largest`.
    pub fn contains(&self, number: u32, largest: u32) -> bool {
        self.0.iter().any(|(from, to)| {
            let (a, b) = (from.resolve(largest), to.resolve(largest));
            (a.min(b)..=a.max(b)).contains(&number)
        })
    }

    /// Whether the set is or holds `$`.
    pub fn uses_saved(&self) -> bool {
        self.0.iter().any(|(from, to)| *from == SeqNum::Saved || *to == SeqNum::Saved)
    }

    /// The ranges with `*` resolved, each with the smaller number first.
    pub fn ranges(&self, largest: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.0.iter().map(move |(from, to)| {
            let (a, b) = (from.resolve(largest), to.resolve(largest));
            (a.min(b), a.max(b))
        })
    }

    /// The set sorted and merged once, for asking about many numbers: [`SequenceSet::contains`]
    /// walks every range on each call.
    pub fn sorted(&self, largest: u32) -> SortedSet {
        let mut ranges: Vec<(u32, u32)> = self.ranges(largest).collect();
        ranges.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
        for (from, to) in ranges {
            match merged.last_mut() {
                Some(last) if from <= last.1.saturating_add(1) => last.1 = last.1.max(to),
                _ => merged.push((from, to)),
            }
        }
        SortedSet(merged)
    }

    /// Whether the set names `*` anywhere.
    pub fn uses_largest(&self) -> bool {
        self.0.iter().any(|(from, to)| *from == SeqNum::Largest || *to == SeqNum::Largest)
    }
}

/// A [`SequenceSet`] with `*` resolved, as sorted ranges that neither overlap nor touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortedSet(Vec<(u32, u32)>);

impl SortedSet {
    pub fn contains(&self, number: u32) -> bool {
        let at = self.0.partition_point(|(_, to)| *to < number);
        self.0.get(at).is_some_and(|(from, _)| *from <= number)
    }

    /// The parts of sorted, non-overlapping inclusive `ranges` that are in the set.
    pub fn intersect(&self, ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        let (mut i, mut j) = (0, 0);
        while i < self.0.len() && j < ranges.len() {
            let (from, to) = (self.0[i].0.max(ranges[j].0), self.0[i].1.min(ranges[j].1));
            if from <= to {
                out.push((from, to));
            }
            if self.0[i].1 < ranges[j].1 { i += 1 } else { j += 1 }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub tag: String,
    pub body: CommandBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandBody {
    Capability,
    Noop,
    Logout,
    StartTls,
    Id,
    Login {
        username: String,
        password: String,
    },
    Authenticate {
        mechanism: String,
        initial: Option<String>,
    },
    Enable(Vec<String>),
    Namespace,
    Select {
        mailbox: String,
        read_only: bool,
        condstore: bool,
        qresync: Option<QresyncParams>,
    },
    Create {
        mailbox: String,
    },
    Delete {
        mailbox: String,
    },
    Rename {
        from: String,
        to: String,
    },
    Subscribe {
        mailbox: String,
    },
    Unsubscribe {
        mailbox: String,
    },
    List(ListCommand),
    Lsub {
        reference: String,
        pattern: String,
    },
    Status {
        mailbox: String,
        items: Vec<StatusItem>,
    },
    Append {
        mailbox: String,
        flags: Vec<String>,
        date: Option<i64>,
        message: Vec<u8>,
    },
    Idle,
    Check,
    Close,
    Unselect,
    Expunge {
        uids: Option<SequenceSet>,
    },
    Search {
        uid: bool,
        returns: Option<Vec<SearchReturn>>,
        criteria: SearchKey,
    },
    Fetch {
        uid: bool,
        set: SequenceSet,
        items: Vec<FetchItem>,
        changed_since: Option<u64>,
        vanished: bool,
    },
    Store {
        uid: bool,
        set: SequenceSet,
        unchanged_since: Option<u64>,
        action: StoreAction,
        silent: bool,
        flags: Vec<String>,
    },
    Copy {
        uid: bool,
        set: SequenceSet,
        mailbox: String,
    },
    Move {
        uid: bool,
        set: SequenceSet,
        mailbox: String,
    },
    GetQuota {
        root: String,
    },
    GetQuotaRoot {
        mailbox: String,
    },
    /// Back to not being logged in (RFC 8437).
    Unauthenticate,
    // Access control lists (RFC 4314).
    GetAcl {
        mailbox: String,
    },
    SetAcl {
        mailbox: String,
        identifier: String,
        rights: String,
    },
    DeleteAcl {
        mailbox: String,
        identifier: String,
    },
    ListRights {
        mailbox: String,
        identifier: String,
    },
    MyRights {
        mailbox: String,
    },
}

/// SELECT's QRESYNC parameter: what the client knew about the mailbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QresyncParams {
    pub uid_validity: u32,
    pub modseq: u64,
    pub known_uids: Option<SequenceSet>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListCommand {
    pub reference: String,
    pub patterns: Vec<String>,
    /// `LSUB`-like: only subscribed mailboxes.
    pub subscribed: bool,
    /// Only mailboxes with a special use.
    pub special_use: bool,
    pub return_subscribed: bool,
    pub return_children: bool,
    pub return_special_use: bool,
    pub return_status: Option<Vec<StatusItem>>,
    /// Plain `LIST` without extended syntax, which answers `\HasChildren` and special uses anyway.
    pub extended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusItem {
    Messages,
    Recent,
    UidNext,
    UidValidity,
    Unseen,
    Size,
    Deleted,
    HighestModSeq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreAction {
    Replace,
    Add,
    Remove,
}

/// A part of a message: numbers of nested body parts, then what of that part.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Section {
    pub part: Vec<u32>,
    pub text: Option<SectionText>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionText {
    Header,
    HeaderFields(Vec<String>),
    HeaderFieldsNot(Vec<String>),
    Text,
    Mime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchItem {
    Envelope,
    Flags,
    InternalDate,
    Rfc822,
    Rfc822Header,
    Rfc822Size,
    Rfc822Text,
    Body,
    BodyStructure,
    Uid,
    ModSeq,
    BodySection {
        section: Section,
        partial: Option<(u32, u32)>,
        peek: bool,
    },
    /// A part with its content transfer encoding undone (RFC 3516).
    Binary {
        part: Vec<u32>,
        partial: Option<(u32, u32)>,
        peek: bool,
    },
    BinarySize {
        part: Vec<u32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchReturn {
    Min,
    Max,
    All,
    Count,
    /// Keep the result for `$` (RFC 5182).
    Save,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchKey {
    All,
    And(Vec<SearchKey>),
    Or(Box<SearchKey>, Box<SearchKey>),
    Not(Box<SearchKey>),
    SequenceSet(SequenceSet),
    Uid(SequenceSet),
    /// Has the keyword (IMAP flags are mapped to keywords already).
    Keyword(String),
    Unkeyword(String),
    New,
    Old,
    Recent,
    Bcc(String),
    Cc(String),
    From(String),
    To(String),
    Subject(String),
    Body(String),
    Text(String),
    Header(String, String),
    /// Received date, as days since 1970-01-01.
    Before(i64),
    On(i64),
    Since(i64),
    /// Date header, as days since 1970-01-01.
    SentBefore(i64),
    SentOn(i64),
    SentSince(i64),
    Larger(u64),
    Smaller(u64),
    ModSeq(u64),
    /// Received within the last so many seconds (WITHIN).
    Younger(u64),
    Older(u64),
    /// A message-number (`uid: false`) or UID set with `*` resolved, built by SEARCH before the
    /// matching so a long set is not walked once per message. The parser never makes it.
    Sorted { uid: bool, set: SortedSet },
}

#[cfg(test)]
mod tests {
    use super::{SeqNum, SequenceSet};

    #[test]
    fn a_sorted_set_matches_like_the_set() {
        use SeqNum::{Largest, Value};
        // Overlapping, touching, reversed and `*` ranges.
        let set = SequenceSet(vec![
            (Value(20), Value(15)),
            (Value(3), Value(5)),
            (Value(4), Value(9)),
            (Value(10), Value(10)),
            (Largest, Value(40)),
            (Value(2), Value(2)),
        ]);
        let sorted = set.sorted(30);
        for number in 0..60 {
            assert_eq!(sorted.contains(number), set.contains(number, 30), "{number}");
        }
        assert_eq!(sorted, super::SortedSet(vec![(2, 10), (15, 20), (30, 40)]));
        assert!(set.uses_largest() && !SequenceSet(vec![(Value(1), Value(2))]).uses_largest());
        assert_eq!(sorted.intersect(&[(1, 3), (8, 16), (25, 50)]), vec![(2, 3), (8, 10), (15, 16), (30, 40)]);
    }
}
