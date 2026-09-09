//! Bounded decode/analysis overlap. A rendezvous channel allows exactly one
//! next frame to be prepared while the consumer analyses the current frame.
use std::{
    sync::mpsc::sync_channel,
    time::{Duration, Instant},
};

#[derive(Debug, Default)]
pub struct FrameTimings {
    pub decode: Duration,
    pub waiting: Duration,
    pub analysis: Duration,
}

/// Decode in order on a helper thread; consume in order on the calling thread.
/// The AI engine therefore never crosses threads and tracking retains frame
/// adjacency. Decode failures can be carried in T and handled by the consumer.
pub fn consume_frames<I, T, E>(
    entries: &[I],
    prefetch: bool,
    mut decode: impl FnMut(&I) -> T + Send,
    mut consume: impl FnMut(T) -> Result<(), E>,
) -> Result<FrameTimings, E>
where
    I: Sync,
    T: Send,
{
    if !prefetch || entries.len() < 2 {
        let mut timings = FrameTimings::default();
        for entry in entries {
            let start = Instant::now();
            let frame = decode(entry);
            let elapsed = start.elapsed();
            timings.decode += elapsed;
            timings.waiting += elapsed;
            let start = Instant::now();
            consume(frame)?;
            timings.analysis += start.elapsed();
        }
        return Ok(timings);
    }
    std::thread::scope(|scope| {
        // Zero queued frames: after handing off frame N, the decoder can
        // prepare N+1, but cannot start N+2 until N+1 has been received.
        let (sender, receiver) = sync_channel(0);
        let producer = scope.spawn(move || {
            let mut decode_time = Duration::ZERO;
            for entry in entries {
                let start = Instant::now();
                let frame = decode(entry);
                decode_time += start.elapsed();
                if sender.send(frame).is_err() {
                    break;
                }
            }
            decode_time
        });
        let mut timings = FrameTimings::default();
        let result = (|| {
            loop {
                let start = Instant::now();
                let next = receiver.recv();
                timings.waiting += start.elapsed();
                let Ok(frame) = next else {
                    break;
                };
                let start = Instant::now();
                consume(frame)?;
                timings.analysis += start.elapsed();
            }
            Ok(())
        })();
        // Also unblock a pending send when analysis exits early with an error.
        drop(receiver);
        timings.decode = match producer.join() {
            Ok(elapsed) => elapsed,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        result.map(|()| timings)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::channel,
        Arc,
    };

    #[test]
    fn preserves_all_outputs_order_and_decode_errors() {
        for prefetch in [false, true] {
            let mut received = Vec::new();
            let result = consume_frames(
                &[0, 1, 2, 3],
                prefetch,
                |&i| if i == 2 { Err(i) } else { Ok(i) },
                |frame| {
                    received.push(frame);
                    Ok::<_, ()>(())
                },
            );
            assert!(result.is_ok());
            assert_eq!(received, vec![Ok(0), Ok(1), Err(2), Ok(3)]);
        }
    }

    #[test]
    fn prepares_next_during_analysis_but_cannot_run_further_ahead() {
        let decoded = Arc::new(AtomicUsize::new(0));
        let producer_count = Arc::clone(&decoded);
        let (next_started, next_signal) = channel();
        consume_frames(
            &[0, 1, 2],
            true,
            move |&i| {
                producer_count.fetch_add(1, Ordering::SeqCst);
                if i == 1 {
                    next_started.send(()).unwrap();
                }
                i
            },
            |i| {
                if i == 0 {
                    // The first frame is still being consumed. The producer must
                    // start the second, and cannot reach the third before return.
                    next_signal.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert_eq!(decoded.load(Ordering::SeqCst), 2);
                }
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(decoded.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn early_analysis_error_unblocks_producer_and_joins_it() {
        let count = AtomicUsize::new(0);
        let result = consume_frames(
            &[0, 1, 2, 3, 4],
            true,
            |&i| {
                count.fetch_add(1, Ordering::SeqCst);
                i
            },
            |_| Err::<(), _>("analysis failed"),
        );
        assert_eq!(result.unwrap_err(), "analysis failed");
        assert!(count.load(Ordering::SeqCst) <= 2);
    }

    #[test]
    fn empty_plan_produces_nothing() {
        consume_frames::<u8, (), ()>(
            &[],
            true,
            |_| panic!("unexpected decode"),
            |_| panic!("unexpected analysis"),
        )
        .unwrap();
    }
}
