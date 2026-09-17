//! Reads a migrated library back through the application's own repository
//! layer, to prove the move produced data the app can actually use rather than
//! just rows with matching counts.
//!
//! ```text
//! cargo run -p skwad-database --example verify_migration -- <postgres url>
//! cargo run -p skwad-database --example verify_migration -- <library folder>
//! ```
//!
//! Given a library folder rather than a URL it resolves the connection exactly
//! the way the app does at launch — `database.json` plus the password file —
//! which makes it the quickest way to answer "can the app open this library?"

use skwad_database::{blob_to_vec, repo, Database, Db, PgConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let target = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("SKWAD_DATABASE_URL").ok())
        .ok_or("usage: verify_migration <postgres url | library folder>")?;

    let config = if target.starts_with("postgres://") || target.starts_with("postgresql://") {
        PgConfig::from_url(&target)?
    } else {
        PgConfig::resolve(std::path::Path::new(&target))
    };

    let db = Database::connect(config)?;
    let mut conn = db.conn()?;
    println!("connected to {}\n", db.describe());

    // --- the listings the app opens with ---------------------------------
    let summaries = repo::shoots::list_summaries(&mut conn)?;
    println!("shoots ({}):", summaries.len());
    for s in &summaries {
        println!(
            "  [{}] {:<20} {:>5} photos {:>4} videos {:>6} faces {:>3} people {:>4} unknown clusters",
            s.shoot.id,
            s.shoot.name,
            s.photo_count,
            s.video_count,
            s.face_count,
            s.person_count,
            s.unknown_cluster_count
        );
    }

    let people = repo::people::list_summaries(&mut conn, None)?;
    println!("\nplayers ({}):", people.len());
    for p in &people {
        println!(
            "  [{}] {:<14} {:>4} samples {:>5} files {:>2} shoots",
            p.person.id, p.person.name, p.face_sample_count, p.media_count, p.shoot_count
        );
    }

    // --- the embeddings recognition actually runs on ----------------------
    let library = repo::faces::library_vectors(&mut conn)?;
    println!("\nconfirmed reference vectors: {}", library.len());
    let mut checked = 0usize;
    for v in library.iter().take(5) {
        let norm = v.embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        let finite = v.embedding.iter().all(|x| x.is_finite());
        println!(
            "  face {:<6} dim={:<4} L2={:.4} finite={}",
            v.face_id,
            v.embedding.len(),
            norm,
            finite
        );
        assert!(finite, "face {} decoded to a non-finite value", v.face_id);
        assert_eq!(v.embedding.len(), 512, "ArcFace vectors are 512-d");
        checked += 1;
    }

    // Every stored blob must decode to a whole number of f32s.
    let rows = conn.rows(
        "SELECT id, embedding, embedding_dim FROM faces WHERE embedding IS NOT NULL",
        skwad_database::params![],
    )?;
    let mut bad = 0usize;
    for row in &rows {
        let id: i64 = row.get(0);
        let blob: Vec<u8> = row.get(1);
        let dim: Option<i64> = row.get(2);
        match blob_to_vec(&blob) {
            Some(v) if Some(v.len() as i64) == dim && v.iter().all(|x| x.is_finite()) => {}
            _ => {
                bad += 1;
                if bad <= 3 {
                    eprintln!("  !! face {id} has an unreadable embedding");
                }
            }
        }
    }
    println!("decoded {} embeddings, {bad} unreadable", rows.len());

    // --- the grid, the albums, the groups ---------------------------------
    for s in &summaries {
        let media = repo::media::query(
            &mut conn,
            &skwad_database::models::MediaQuery {
                shoot_id: Some(s.shoot.id),
                limit: Some(5),
                ..Default::default()
            },
        )?;
        let albums = repo::albums::list(&mut conn, s.shoot.id)?;
        let groups = repo::groups::list(&mut conn, s.shoot.id)?;
        println!(
            "\nshoot {} — first files: {:?}",
            s.shoot.id,
            media.iter().map(|m| m.filename.as_str()).collect::<Vec<_>>()
        );
        println!(
            "  albums: {}   groups: {}",
            albums
                .iter()
                .map(|a| format!("{}({})", a.name, a.media_count))
                .collect::<Vec<_>>()
                .join(", "),
            groups
                .iter()
                .map(|g| format!("{}({})", g.name, g.media_count))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let roster = repo::roster::list(&mut conn)?;
    println!("\nroster entries: {}", roster.len());
    if let Some(entry) = roster.first() {
        println!("  e.g. {} / {} — {}", entry.ign, entry.player_name, entry.team);
    }

    let projects = repo::projects::list_accessible(&mut conn, "", "", None)?;
    println!("projects visible to an anonymous caller: {}", projects.len());

    println!("\nchecked {checked} vectors in detail; {bad} bad embeddings overall.");
    if bad > 0 {
        return Err("some embeddings did not survive the migration".into());
    }
    println!("OK — the migrated library reads correctly through the app's own queries.");
    Ok(())
}
