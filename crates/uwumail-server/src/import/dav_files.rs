//! Calendars and address books from files: `uwumail-server import ics|vcf`, the same way the
//! portal takes them (docs/calendar-import.md).

use std::io::Read;
use std::path::Path;

use uwumail_smtp::Language;
use uwumail_store::{
    DavCollection, DavImportMode, DavKind, NewDavCollection, NewImportCollection, Store, decode_text, split_ics,
    split_vcf,
};

/// Where the entries of a file go.
pub struct DavTarget {
    pub account: String,
    /// The URL name of an existing collection; a new one when `None`.
    pub collection: Option<String>,
    pub name: Option<String>,
    pub only_new: bool,
    pub dry_run: bool,
}

/// Reads at most this much; the portal takes no more either.
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

pub async fn dav_file(
    store: &Store,
    language: Language,
    kind: DavKind,
    file: &Path,
    target: DavTarget,
) -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    if file.as_os_str() == "-" {
        std::io::stdin().take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    } else {
        std::fs::File::open(file)?.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        anyhow::bail!("the file is larger than {} MB", MAX_FILE_BYTES / 1024 / 1024);
    }
    let text = decode_text(&bytes);
    let split = match kind {
        DavKind::Calendar => split_ics(&text, false),
        DavKind::Addressbook => split_vcf(&text),
    };
    if !split.recognized {
        anyhow::bail!(
            "this is not {}",
            if kind == DavKind::Calendar { "an iCalendar file (.ics)" } else { "a vCard file (.vcf)" }
        );
    }
    let account = store
        .account(&target.account.trim().to_lowercase())
        .await?
        .filter(|account| account.deleted_at.is_none())
        .ok_or_else(|| anyhow::anyhow!("nobody here logs in as {}", target.account))?;
    let (calendar_name, book_name) = language.collection_names();
    let default = match kind {
        DavKind::Calendar => NewDavCollection::default_calendar(calendar_name),
        DavKind::Addressbook => NewDavCollection::default_address_book(book_name),
    };
    let what = if kind == DavKind::Calendar { "entries" } else { "cards" };
    for problem in &split.problems {
        println!("  left out: {} ({})", problem.item, problem.reason);
    }
    if target.dry_run {
        println!(
            "Would import {} {what} into {}.",
            split.objects.len(),
            target.collection.as_deref().unwrap_or("a new one")
        );
        return Ok(());
    }
    let collection: DavCollection = match &target.collection {
        Some(slug) => {
            let collections = store.dav_collections(account.id, kind, default).await?;
            collections
                .into_iter()
                .find(|collection| collection.slug == *slug)
                .ok_or_else(|| anyhow::anyhow!("{} has nothing called {slug}", account.login))?
        }
        None => {
            let file_name = file.file_stem().and_then(|stem| stem.to_str()).filter(|_| file.as_os_str() != "-");
            let name = target
                .name
                .clone()
                .or_else(|| split.meta.name.clone())
                .or_else(|| file_name.map(str::to_owned))
                .unwrap_or_else(|| default.display_name.clone());
            let new = NewImportCollection {
                name,
                description: split.meta.description.clone().unwrap_or_default(),
                color: split.meta.color.clone(),
            };
            store.dav_create_import_collection(account.id, kind, new, default).await?
        }
    };
    let mode = if target.only_new { DavImportMode::OnlyNew } else { DavImportMode::Merge };
    let report = store.dav_import(account.id, collection.id, split.objects, mode).await?;
    for problem in &report.problems {
        println!("  left out: {} ({})", problem.item, problem.reason);
    }
    if report.truncated {
        println!("  … and more");
    }
    println!(
        "{}/{}: {} new, {} changed, {} the same, {} left out.",
        account.login, collection.slug, report.created, report.updated, report.unchanged, report.skipped
    );
    crate::commands::audit(
        store,
        if kind == DavKind::Calendar { "import.ics" } else { "import.vcf" },
        &account.login,
        serde_json::json!({ "collection": collection.slug, "created": report.created, "updated": report.updated }),
    )
    .await;
    Ok(())
}
