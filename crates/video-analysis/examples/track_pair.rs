//! Small calibration utility for comparing one known face box across two
//! sampled frames. It does not modify media or the application database.

use std::env;
use std::path::Path;
use std::time::Instant;

use teo_video_analysis::tracking::{self, TrackBox};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<String> = env::args().collect();
    if arguments.len() != 7 {
        return Err("usage: track_pair <previous-image> <current-image> <x> <y> <width> <height>".into());
    }
    let previous = image::open(Path::new(&arguments[1]))?.to_rgb8();
    let current = image::open(Path::new(&arguments[2]))?.to_rgb8();
    let bbox = TrackBox {
        x: arguments[3].parse()?,
        y: arguments[4].parse()?,
        width: arguments[5].parse()?,
        height: arguments[6].parse()?,
    };

    let started = Instant::now();
    let proposal = tracking::track_boxes(&previous, &current, &[bbox])[0];
    println!("backend={:?}", tracking::backend());
    println!("elapsed_ms={:.3}", started.elapsed().as_secs_f64() * 1_000.0);
    println!("proposal={proposal:?}");
    Ok(())
}
