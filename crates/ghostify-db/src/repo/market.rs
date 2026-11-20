//! Local cache of the marketplace index with full-text search, plus the
//! user's own ratings.

use super::{col, now};
use crate::db::SqlExt;
use ghostify_core::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

string_enum! {
    ListingKind {
        Plugin => "plugin",
        Theme => "theme",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Listing {
    pub id: String,
    pub kind: ListingKind,
    pub name: String,
    pub author: String,
    pub repo: String,
    pub category: String,
    pub description: String,
    pub tags: Vec<String>,
    pub latest_version: String,
    pub stars: i64,
    pub rating_avg: Option<f64>,
    pub rating_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rating {
    pub listing_id: String,
    pub score: u8,
    pub github_reaction_id: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryCount {
    pub category: String,
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Free text; every word is matched as a prefix against name, description and tags.
    pub text: String,
    pub kind: Option<ListingKind>,
    pub category: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

impl Default for SearchQuery {
    fn default() -> Self {
        SearchQuery {
            text: String::new(),
            kind: None,
            category: None,
            limit: 50,
            offset: 0,
        }
    }
}

const MAX_LIMIT: u32 = 200;
const SELECT: &str = "l.id, l.kind, l.name, l.author, l.repo, l.category, l.description, l.tags,
                      l.latest_version, l.stars, l.rating_avg, l.rating_count";

fn map(row: &Row<'_>) -> rusqlite::Result<Listing> {
    let tags: String = row.get(7)?;
    Ok(Listing {
        id: row.get(0)?,
        kind: col(ListingKind::parse(&row.get::<_, String>(1)?))?,
        name: row.get(2)?,
        author: row.get(3)?,
        repo: row.get(4)?,
        category: row.get(5)?,
        description: row.get(6)?,
        tags: col(serde_json::from_str(&tags).map_err(Error::from))?,
        latest_version: row.get(8)?,
        stars: row.get(9)?,
        rating_avg: row.get(10)?,
        rating_count: row.get(11)?,
    })
}

/// Stores a freshly downloaded index: inserts or updates every listing and
/// removes the ones that are no longer part of it. Returns the number of
/// listings stored. Runs in one transaction, so readers never see a partial
/// index.
pub fn replace_index(conn: &Connection, listings: &[Listing], index_version: i64) -> Result<usize> {
    let tx = conn.unchecked_transaction().db()?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO market_listings(id, kind, name, author, repo, category, description, tags,
                                             latest_version, stars, rating_avg, rating_count, index_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(id) DO UPDATE SET
                     kind = excluded.kind, name = excluded.name, author = excluded.author,
                     repo = excluded.repo, category = excluded.category,
                     description = excluded.description, tags = excluded.tags,
                     latest_version = excluded.latest_version, stars = excluded.stars,
                     rating_avg = excluded.rating_avg, rating_count = excluded.rating_count,
                     index_version = excluded.index_version",
            )
            .db()?;
        for listing in listings {
            if listing.id.trim().is_empty() || listing.name.trim().is_empty() {
                return Err(Error::Invalid(
                    "listing id and name must not be empty".into(),
                ));
            }
            let tags = serde_json::to_string(&listing.tags)?;
            stmt.execute(params![
                listing.id,
                listing.kind.as_str(),
                listing.name,
                listing.author,
                listing.repo,
                listing.category,
                listing.description,
                tags,
                listing.latest_version,
                listing.stars,
                listing.rating_avg,
                listing.rating_count,
                index_version,
            ])
            .db()?;
        }
    }
    tx.execute(
        "DELETE FROM market_listings WHERE index_version <> ?1",
        [index_version],
    )
    .db()?;
    tx.commit().db()?;
    Ok(listings.len())
}

/// Download information of one listing's latest version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub listing_id: String,
    pub version: String,
    pub download_url: String,
    pub sha256: String,
    pub size: u64,
    pub permissions: Vec<String>,
}

/// Stores the package rows of an index and removes those of older indexes.
/// Call it in the same connection scope as [`replace_index`] with the same version.
pub fn replace_packages(conn: &Connection, packages: &[Package], index_version: i64) -> Result<()> {
    let tx = conn.unchecked_transaction().db()?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO market_packages(listing_id, version, download_url, sha256, size, permissions, index_version)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(listing_id) DO UPDATE SET
                     version = excluded.version, download_url = excluded.download_url,
                     sha256 = excluded.sha256, size = excluded.size,
                     permissions = excluded.permissions, index_version = excluded.index_version",
            )
            .db()?;
        for package in packages {
            let size = i64::try_from(package.size)
                .map_err(|_| Error::Invalid("package size is out of range".into()))?;
            stmt.execute(params![
                package.listing_id,
                package.version,
                package.download_url,
                package.sha256,
                size,
                serde_json::to_string(&package.permissions)?,
                index_version,
            ])
            .db()?;
        }
    }
    tx.execute(
        "DELETE FROM market_packages WHERE index_version <> ?1",
        [index_version],
    )
    .db()?;
    tx.commit().db()?;
    Ok(())
}

pub fn get_package(conn: &Connection, listing_id: &str) -> Result<Option<Package>> {
    conn.query_row(
        "SELECT listing_id, version, download_url, sha256, size, permissions FROM market_packages WHERE listing_id = ?1",
        [listing_id],
        |r| {
            let permissions: String = r.get(5)?;
            Ok(Package {
                listing_id: r.get(0)?,
                version: r.get(1)?,
                download_url: r.get(2)?,
                sha256: r.get(3)?,
                size: r.get::<_, i64>(4)?.max(0) as u64,
                permissions: serde_json::from_str(&permissions).unwrap_or_default(),
            })
        },
    )
    .optional()
    .db()
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Listing>> {
    conn.query_row(
        &format!("SELECT {SELECT} FROM market_listings l WHERE l.id = ?1"),
        [id],
        map,
    )
    .optional()
    .db()
}

/// Turns user input into a safe FTS5 expression: only alphanumeric words,
/// each quoted and used as a prefix.
fn fts_expression(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| format!("\"{word}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// Searches the cached index. With text the best matches come first, without
/// text the most starred listings do.
pub fn search(conn: &Connection, query: &SearchQuery) -> Result<Vec<Listing>> {
    let limit = query.limit.clamp(1, MAX_LIMIT);
    let kind = query.kind.map(|k| k.as_str());
    let category = query.category.as_deref();
    let rows = match fts_expression(&query.text) {
        Some(expr) => {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {SELECT} FROM market_listings l
                     JOIN market_fts ON market_fts.rowid = l.rowid
                     WHERE market_fts MATCH ?1
                       AND (?2 IS NULL OR l.kind = ?2)
                       AND (?3 IS NULL OR l.category = ?3)
                     ORDER BY bm25(market_fts), l.stars DESC, l.name
                     LIMIT ?4 OFFSET ?5"
                ))
                .db()?;
            let mapped = stmt
                .query_map(params![expr, kind, category, limit, query.offset], map)
                .db()?;
            mapped.collect::<rusqlite::Result<Vec<_>>>().db()?
        }
        None => {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {SELECT} FROM market_listings l
                     WHERE (?1 IS NULL OR l.kind = ?1) AND (?2 IS NULL OR l.category = ?2)
                     ORDER BY l.stars DESC, l.name
                     LIMIT ?3 OFFSET ?4"
                ))
                .db()?;
            let mapped = stmt
                .query_map(params![kind, category, limit, query.offset], map)
                .db()?;
            mapped.collect::<rusqlite::Result<Vec<_>>>().db()?
        }
    };
    Ok(rows)
}

/// Categories with their number of listings, largest first.
pub fn categories(conn: &Connection, kind: Option<ListingKind>) -> Result<Vec<CategoryCount>> {
    let mut stmt = conn
        .prepare(
            "SELECT category, COUNT(*) FROM market_listings
             WHERE (?1 IS NULL OR kind = ?1) GROUP BY category ORDER BY COUNT(*) DESC, category",
        )
        .db()?;
    let rows = stmt
        .query_map([kind.map(|k| k.as_str())], |r| {
            Ok(CategoryCount {
                category: r.get(0)?,
                count: r.get(1)?,
            })
        })
        .db()?;
    rows.collect::<rusqlite::Result<Vec<_>>>().db()
}

// ---- the user's own ratings ---------------------------------------------

pub fn set_rating(
    conn: &Connection,
    listing_id: &str,
    score: u8,
    reaction_id: Option<&str>,
) -> Result<Rating> {
    if !(1..=5).contains(&score) {
        return Err(Error::Invalid("rating must be between 1 and 5".into()));
    }
    let created_at = now();
    conn.execute(
        "INSERT INTO market_ratings(listing_id, score, github_reaction_id, created_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(listing_id) DO UPDATE SET
             score = excluded.score, github_reaction_id = excluded.github_reaction_id,
             created_at = excluded.created_at",
        params![listing_id, score, reaction_id, created_at],
    )
    .db()?;
    Ok(Rating {
        listing_id: listing_id.to_owned(),
        score,
        github_reaction_id: reaction_id.map(str::to_owned),
        created_at,
    })
}

pub fn get_rating(conn: &Connection, listing_id: &str) -> Result<Option<Rating>> {
    conn.query_row(
        "SELECT listing_id, score, github_reaction_id, created_at FROM market_ratings WHERE listing_id = ?1",
        [listing_id],
        |r| {
            Ok(Rating {
                listing_id: r.get(0)?,
                score: r.get(1)?,
                github_reaction_id: r.get(2)?,
                created_at: r.get(3)?,
            })
        },
    )
    .optional()
    .db()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_are_stored_and_replaced_per_index_version() {
        let db = crate::Db::open_in_memory().unwrap();
        let package = |id: &str, version: &str| Package {
            listing_id: id.into(),
            version: version.into(),
            download_url: format!("https://github.com/x/{id}.zip"),
            sha256: "a".repeat(64),
            size: 1234,
            permissions: vec!["storage".into()],
        };
        db.with(|c| {
            replace_packages(c, &[package("a", "1.0.0"), package("b", "2.0.0")], 1)?;
            assert_eq!(get_package(c, "a")?.unwrap().version, "1.0.0");
            replace_packages(c, &[package("a", "1.1.0")], 2)?;
            assert_eq!(get_package(c, "a")?.unwrap().version, "1.1.0");
            assert!(
                get_package(c, "b")?.is_none(),
                "packages missing from the new index are removed"
            );
            assert_eq!(get_package(c, "a")?.unwrap().permissions, ["storage"]);
            Ok(())
        })
        .unwrap();
    }
    use crate::Db;

    fn listing(
        id: &str,
        kind: ListingKind,
        name: &str,
        category: &str,
        description: &str,
        stars: i64,
    ) -> Listing {
        Listing {
            id: id.into(),
            kind,
            name: name.into(),
            author: "someone".into(),
            repo: format!("https://github.com/{id}"),
            category: category.into(),
            description: description.into(),
            tags: vec!["spotify".into(), "extra".into()],
            latest_version: "1.0.0".into(),
            stars,
            rating_avg: Some(4.5),
            rating_count: 10,
        }
    }

    fn sample() -> Vec<Listing> {
        vec![
            listing(
                "a/lyrics-plus",
                ListingKind::Plugin,
                "Lyrics Plus",
                "lyrics",
                "Synced lyrics in the sidebar",
                900,
            ),
            listing(
                "b/visualizer",
                ListingKind::Plugin,
                "Audio Visualizer",
                "visuals",
                "Spectrum bars and waves",
                300,
            ),
            listing(
                "c/dark-night",
                ListingKind::Theme,
                "Dark Night",
                "themes",
                "A dark theme with blue accents",
                1200,
            ),
            listing(
                "d/lyrics-lite",
                ListingKind::Plugin,
                "Lyrics Lite",
                "lyrics",
                "Minimal lyrics overlay",
                50,
            ),
        ]
    }

    #[test]
    fn replace_index_and_get() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            assert_eq!(replace_index(c, &sample(), 1)?, 4);
            let found = get(c, "c/dark-night")?.unwrap();
            assert_eq!(found.kind, ListingKind::Theme);
            assert_eq!(found.tags, ["spotify", "extra"]);
            assert_eq!(get(c, "nope")?, None);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn newer_index_removes_stale_listings_and_updates_search() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            replace_index(c, &sample(), 1)?;
            let mut next = sample();
            next.remove(3);
            next[0].description = "Karaoke style words".into();
            replace_index(c, &next, 2)?;

            assert_eq!(get(c, "d/lyrics-lite")?, None);
            let hits = search(
                c,
                &SearchQuery {
                    text: "karaoke".into(),
                    ..Default::default()
                },
            )?;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].id, "a/lyrics-plus");
            let stale = search(
                c,
                &SearchQuery {
                    text: "sidebar".into(),
                    ..Default::default()
                },
            )?;
            assert!(stale.is_empty(), "removed text must leave the FTS index");
            let removed = search(
                c,
                &SearchQuery {
                    text: "minimal".into(),
                    ..Default::default()
                },
            )?;
            assert!(removed.is_empty());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn search_matches_prefixes_and_applies_filters() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            replace_index(c, &sample(), 1)?;
            let hits = search(
                c,
                &SearchQuery {
                    text: "lyr".into(),
                    ..Default::default()
                },
            )?;
            let ids: Vec<_> = hits.iter().map(|l| l.id.as_str()).collect();
            assert_eq!(ids.len(), 2);
            assert!(ids.contains(&"a/lyrics-plus") && ids.contains(&"d/lyrics-lite"));

            let themes = search(
                c,
                &SearchQuery {
                    kind: Some(ListingKind::Theme),
                    ..Default::default()
                },
            )?;
            assert_eq!(themes.len(), 1);
            let visuals = search(
                c,
                &SearchQuery {
                    category: Some("visuals".into()),
                    ..Default::default()
                },
            )?;
            assert_eq!(visuals[0].id, "b/visualizer");
            let none = search(
                c,
                &SearchQuery {
                    text: "lyrics".into(),
                    kind: Some(ListingKind::Theme),
                    ..Default::default()
                },
            )?;
            assert!(none.is_empty());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn browsing_without_text_orders_by_stars_and_paginates() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            replace_index(c, &sample(), 1)?;
            let all = search(c, &SearchQuery::default())?;
            let ids: Vec<_> = all.iter().map(|l| l.id.as_str()).collect();
            assert_eq!(
                ids,
                [
                    "c/dark-night",
                    "a/lyrics-plus",
                    "b/visualizer",
                    "d/lyrics-lite"
                ]
            );
            let page = search(
                c,
                &SearchQuery {
                    limit: 2,
                    offset: 1,
                    ..Default::default()
                },
            )?;
            assert_eq!(page.len(), 2);
            assert_eq!(page[0].id, "a/lyrics-plus");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn hostile_search_input_cannot_break_the_query() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            replace_index(c, &sample(), 1)?;
            for text in ["\"", "name:*", "lyrics OR", "a' OR '1'='1", "(((", "-- x"] {
                search(
                    c,
                    &SearchQuery {
                        text: text.into(),
                        ..Default::default()
                    },
                )?;
            }
            let punctuation_only = search(
                c,
                &SearchQuery {
                    text: "!!!".into(),
                    ..Default::default()
                },
            )?;
            assert_eq!(punctuation_only.len(), 4, "no words means plain browsing");
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn categories_are_counted() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            replace_index(c, &sample(), 1)?;
            let all = categories(c, None)?;
            assert_eq!(
                all[0],
                CategoryCount {
                    category: "lyrics".into(),
                    count: 2
                }
            );
            let themes = categories(c, Some(ListingKind::Theme))?;
            assert_eq!(
                themes,
                [CategoryCount {
                    category: "themes".into(),
                    count: 1
                }]
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn invalid_listing_rolls_the_whole_index_back() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            replace_index(c, &sample(), 1)?;
            let mut broken = sample();
            broken[2].name = " ".into();
            assert_eq!(
                replace_index(c, &broken, 2).unwrap_err().code(),
                "invalid_input"
            );
            assert_eq!(
                search(c, &SearchQuery::default())?.len(),
                4,
                "old index must stay intact"
            );
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn ratings_survive_index_refresh_and_validate_range() {
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            replace_index(c, &sample(), 1)?;
            set_rating(c, "a/lyrics-plus", 4, Some("reaction-1"))?;
            set_rating(c, "a/lyrics-plus", 5, None)?;
            replace_index(c, &[], 2)?;
            let rating = get_rating(c, "a/lyrics-plus")?.unwrap();
            assert_eq!(rating.score, 5);
            assert_eq!(rating.github_reaction_id, None);
            assert_eq!(
                set_rating(c, "a/lyrics-plus", 0, None).unwrap_err().code(),
                "invalid_input"
            );
            assert_eq!(
                set_rating(c, "a/lyrics-plus", 6, None).unwrap_err().code(),
                "invalid_input"
            );
            assert_eq!(get_rating(c, "none")?, None);
            Ok(())
        })
        .unwrap();
    }
}
