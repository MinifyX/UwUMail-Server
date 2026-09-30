//! BIMI per domain (docs/bimi.md): the logo as SVG Tiny PS and an optional mark certificate,
//! stored as the portal cleaned and checked them, and whether they are published.

use rusqlite::{OptionalExtension, params};

use crate::{Result, Store};

/// What a domain has for BIMI.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainBimi {
    pub enabled: bool,
    pub title: String,
    /// The logo in SVG Tiny PS.
    pub svg: Option<String>,
    pub svg_updated_at: Option<i64>,
    /// The Verified Mark or Common Mark Certificate, PEM with its chain.
    pub certificate: Option<String>,
    pub certificate_updated_at: Option<i64>,
}

/// One change to a domain's BIMI; `None` leaves a part as it is.
#[derive(Debug, Clone, Default)]
pub struct BimiUpdate {
    pub enabled: Option<bool>,
    pub title: Option<String>,
    /// `Some(None)` removes the logo.
    pub svg: Option<Option<String>>,
    /// `Some(None)` removes the certificate.
    pub certificate: Option<Option<String>>,
}

impl Store {
    /// A domain's BIMI setup; the default (off, nothing stored) when it never had one.
    pub async fn domain_bimi(&self, domain_id: i64) -> Result<DomainBimi> {
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT enabled, title, svg, svg_updated_at, certificate, certificate_updated_at
                     FROM domain_bimi WHERE domain_id = ?1",
                    [domain_id],
                    |row| {
                        Ok(DomainBimi {
                            enabled: row.get(0)?,
                            title: row.get(1)?,
                            svg: row.get(2)?,
                            svg_updated_at: row.get(3)?,
                            certificate: row.get(4)?,
                            certificate_updated_at: row.get(5)?,
                        })
                    },
                )
                .optional()?
                .unwrap_or_default())
        })
        .await
    }

    /// Changes a domain's BIMI setup at `at`. Without a logo it cannot stay on.
    pub async fn update_domain_bimi(&self, domain_id: i64, update: BimiUpdate, at: i64) -> Result<DomainBimi> {
        self.write(move |tx| {
            tx.execute("INSERT OR IGNORE INTO domain_bimi (domain_id) VALUES (?1)", [domain_id])?;
            if let Some(title) = &update.title {
                tx.execute("UPDATE domain_bimi SET title = ?1 WHERE domain_id = ?2", params![title, domain_id])?;
            }
            if let Some(svg) = &update.svg {
                tx.execute(
                    "UPDATE domain_bimi SET svg = ?1, svg_updated_at = ?2 WHERE domain_id = ?3",
                    params![svg, svg.as_ref().map(|_| at), domain_id],
                )?;
            }
            if let Some(certificate) = &update.certificate {
                tx.execute(
                    "UPDATE domain_bimi SET certificate = ?1, certificate_updated_at = ?2 WHERE domain_id = ?3",
                    params![certificate, certificate.as_ref().map(|_| at), domain_id],
                )?;
            }
            if let Some(enabled) = update.enabled {
                tx.execute("UPDATE domain_bimi SET enabled = ?1 WHERE domain_id = ?2", params![enabled, domain_id])?;
            }
            tx.execute("UPDATE domain_bimi SET enabled = 0 WHERE domain_id = ?1 AND svg IS NULL", [domain_id])?;
            Ok(())
        })
        .await?;
        self.domain_bimi(domain_id).await
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::store;

    use super::*;

    #[tokio::test]
    async fn bimi_is_kept_per_domain_and_needs_a_logo() {
        let (store, _dir) = store().await;
        let domain = store.create_domain("example.org").await.unwrap();
        assert_eq!(store.domain_bimi(domain.id).await.unwrap(), DomainBimi::default());

        // Switching on without a logo does not stick.
        let bimi = store
            .update_domain_bimi(domain.id, BimiUpdate { enabled: Some(true), ..Default::default() }, 10)
            .await
            .unwrap();
        assert!(!bimi.enabled);

        let update = BimiUpdate {
            enabled: Some(true),
            title: Some("Example".into()),
            svg: Some(Some("<svg/>".into())),
            certificate: Some(Some("-----BEGIN CERTIFICATE-----".into())),
        };
        let bimi = store.update_domain_bimi(domain.id, update, 20).await.unwrap();
        assert!(bimi.enabled);
        assert_eq!((bimi.svg_updated_at, bimi.certificate_updated_at), (Some(20), Some(20)));
        assert_eq!(bimi.title, "Example");

        let bimi = store
            .update_domain_bimi(domain.id, BimiUpdate { certificate: Some(None), ..Default::default() }, 30)
            .await
            .unwrap();
        assert_eq!((bimi.certificate, bimi.certificate_updated_at, bimi.enabled), (None, None, true));
        // Without the logo it is off again.
        let bimi = store
            .update_domain_bimi(domain.id, BimiUpdate { svg: Some(None), ..Default::default() }, 40)
            .await
            .unwrap();
        assert!(!bimi.enabled && bimi.svg.is_none());

        // It goes with the domain.
        store.delete_domain("example.org").await.unwrap();
        assert_eq!(store.domain_bimi(domain.id).await.unwrap(), DomainBimi::default());
    }
}
