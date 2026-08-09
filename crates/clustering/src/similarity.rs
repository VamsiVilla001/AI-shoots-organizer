//! Cosine similarity, and a k-nearest-neighbour search over embeddings.
//!
//! §15 of the plan starts here deliberately: for a player library measured in
//! thousands rather than millions of faces, a parallel linear scan beats the
//! complexity of a vector index. The kNN graph this module produces is the
//! only thing the clusterer needs, so swapping in HNSW later means replacing
//! [`knn_graph`] and nothing else.

use rayon::prelude::*;

/// Dot product of two vectors. Embeddings arrive unit-length, so this *is*
/// their cosine similarity — no division needed.
#[inline]
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>().clamp(-1.0, 1.0)
}

/// Mean of several embeddings, renormalised to unit length.
pub fn centroid(vectors: &[&[f32]]) -> Vec<f32> {
    let Some(dim) = vectors.first().map(|v| v.len()) else {
        return Vec::new();
    };
    let mut sum = vec![0.0f32; dim];
    let mut counted = 0usize;
    for v in vectors {
        if v.len() != dim {
            continue; // a vector from a different model; ignore it
        }
        for (acc, value) in sum.iter_mut().zip(v.iter()) {
            *acc += value;
        }
        counted += 1;
    }
    if counted == 0 {
        return vec![0.0; dim];
    }

    let norm = sum.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 1e-12 {
        for v in sum.iter_mut() {
            *v /= norm;
        }
    }
    sum
}

/// One edge of the neighbour graph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Neighbour {
    pub index: usize,
    pub similarity: f32,
}

/// Above this many faces, an all-pairs scan gets slow enough to be worth
/// warning about in the log.
const LARGE_SET_WARNING: usize = 25_000;

/// For every embedding, its `k` most similar peers scoring at least
/// `min_similarity`. Symmetric by construction, so the clusterer sees an
/// undirected graph.
pub fn knn_graph(embeddings: &[Vec<f32>], k: usize, min_similarity: f32) -> Vec<Vec<Neighbour>> {
    let n = embeddings.len();
    if n < 2 || k == 0 {
        return vec![Vec::new(); n];
    }
    if n > LARGE_SET_WARNING {
        tracing::warn!(faces = n, "clustering a very large face set; this pass may take a while");
    }

    // Each row scans every other embedding but keeps only the best k, so peak
    // memory stays O(n·k) rather than O(n²).
    let mut graph: Vec<Vec<Neighbour>> = (0..n)
        .into_par_iter()
        .map(|i| {
            let mut best: Vec<Neighbour> = Vec::with_capacity(k + 1);
            for j in 0..n {
                if i == j {
                    continue;
                }
                let similarity = cosine(&embeddings[i], &embeddings[j]);
                if similarity < min_similarity {
                    continue;
                }
                if best.len() < k {
                    best.push(Neighbour { index: j, similarity });
                    if best.len() == k {
                        best.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
                    }
                } else if similarity > best[k - 1].similarity {
                    best[k - 1] = Neighbour { index: j, similarity };
                    // The list is short; an insertion pass is cheaper than a
                    // heap and keeps the worst candidate at the end.
                    let mut p = k - 1;
                    while p > 0 && best[p].similarity > best[p - 1].similarity {
                        best.swap(p, p - 1);
                        p -= 1;
                    }
                }
            }
            best.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
            best
        })
        .collect();

    // A is in B's top-k but B might not be in A's; add the missing direction so
    // one-sided popularity does not tear a real group apart.
    let mut additions: Vec<Vec<Neighbour>> = vec![Vec::new(); n];
    for (i, neighbours) in graph.iter().enumerate() {
        for neighbour in neighbours {
            let already = graph[neighbour.index].iter().any(|m| m.index == i);
            if !already {
                additions[neighbour.index].push(Neighbour { index: i, similarity: neighbour.similarity });
            }
        }
    }
    for (row, extra) in graph.iter_mut().zip(additions) {
        row.extend(extra);
    }

    graph
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
    }

    #[test]
    fn cosine_of_a_vector_with_itself_is_one() {
        let v = unit(vec![0.2, -0.5, 0.9]);
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_handles_mismatched_dimensions() {
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn centroid_of_identical_vectors_is_that_vector() {
        let v = unit(vec![1.0, 2.0, 3.0]);
        let c = centroid(&[&v, &v, &v]);
        for (a, b) in c.iter().zip(&v) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn centroid_of_nothing_is_empty() {
        assert!(centroid(&[]).is_empty());
    }

    #[test]
    fn knn_finds_the_closest_peers() {
        // Three tight pairs, well separated from each other.
        let embeddings = vec![
            unit(vec![1.0, 0.0, 0.0]),
            unit(vec![0.99, 0.1, 0.0]),
            unit(vec![0.0, 1.0, 0.0]),
            unit(vec![0.1, 0.99, 0.0]),
        ];
        let graph = knn_graph(&embeddings, 1, 0.5);

        assert_eq!(graph[0][0].index, 1);
        assert_eq!(graph[2][0].index, 3);
        assert!(graph[0].iter().all(|n| n.index != 2), "unrelated faces must not connect");
    }

    #[test]
    fn threshold_can_disconnect_everything() {
        let embeddings = vec![unit(vec![1.0, 0.0]), unit(vec![0.0, 1.0])];
        let graph = knn_graph(&embeddings, 5, 0.9);
        assert!(graph.iter().all(|row| row.is_empty()));
    }

    #[test]
    fn graph_is_symmetric() {
        let embeddings = vec![
            unit(vec![1.0, 0.0, 0.0]),
            unit(vec![0.9, 0.2, 0.0]),
            unit(vec![0.8, 0.4, 0.0]),
        ];
        // k=1 would leave asymmetric edges without the repair pass.
        let graph = knn_graph(&embeddings, 1, 0.0);
        for (i, row) in graph.iter().enumerate() {
            for neighbour in row {
                assert!(
                    graph[neighbour.index].iter().any(|m| m.index == i),
                    "edge {i}->{} has no reverse", neighbour.index
                );
            }
        }
    }

    #[test]
    fn tiny_inputs_do_not_panic() {
        assert!(knn_graph(&[], 5, 0.5).is_empty());
        assert_eq!(knn_graph(&[vec![1.0]], 5, 0.5).len(), 1);
    }
}
