//! IMAP commands as the parser hands them to a session.

/// A number in a sequence set: a value or `*`, the largest number in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqNum {
    Value(u32),
    Largest,
}

impl SeqNum {
    fn resolve(self, largest: u32) -> u32 {
        match self {
            SeqNum::Value(value) => value,
            SeqNum::Largest => largest,
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

    /// The ranges with `*` resolved, each with the smaller number first.
    pub fn ranges(&self, largest: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.0.iter().map(move |(from, to)| {
            let (a, b) = (from.resolve(largest), to.resolve(largest));
            (a.min(b), a.max(b))
        })
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
    BodySection { section: Section, partial: Option<(u32, u32)>, peek: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchReturn {
    Min,
    Max,
    All,
    Count,
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
}
