// A small scoped-thread helper that maps work over chunks of a slice in parallel, keeping order.
use std::sync::atomic::{AtomicUsize, Ordering};

// Apply `f` to consecutive chunks on all cores; `f` returns one result per item of its chunk.
pub fn map_chunks<T: Sync, R: Send>(items: &[T], chunk: usize, f: impl Fn(&[T]) -> Vec<R> + Sync) -> Vec<R> {
    let chunks: Vec<&[T]> = items.chunks(chunk.max(1)).collect();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get()).min(chunks.len());
    if workers <= 1 {
        return chunks.into_iter().flat_map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let mut parts: Vec<(usize, Vec<R>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = vec![];
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(items) = chunks.get(i) else { break };
                        mine.push((i, f(items)));
                    }
                    mine
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("worker thread panicked")).collect()
    });
    parts.sort_by_key(|p| p.0);
    parts.into_iter().flat_map(|p| p.1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Results come back in input order for any chunk size.
    #[test]
    fn test_map_chunks_keeps_order() {
        let items: Vec<u32> = (0..1000).collect();
        for chunk in [1, 7, 64, 5000] {
            let out = map_chunks(&items, chunk, |c| c.iter().map(|x| x * 2).collect());
            assert_eq!(out, items.iter().map(|x| x * 2).collect::<Vec<_>>());
        }
    }

    // An empty input yields an empty output without spawning workers.
    #[test]
    fn test_map_chunks_empty() {
        let out: Vec<u8> = map_chunks(&[] as &[u8], 8, |c| c.to_vec());
        assert!(out.is_empty());
    }
}
