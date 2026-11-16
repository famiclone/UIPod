//! Virtual folder hierarchy reconstructed from Navidrome `server_path` entries.
//!
//! Enables `Main -> Files` in Navidrome mode to browse the exact folder layout
//! of the remote music library with natural alphabetical sorting (`2` before `10`).

use std::cmp::Ordering;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualEntry {
    pub is_dir: bool,
    /// Leaf display name (folder name or filename / title).
    pub name: String,
    /// For directories: normalized relative folder path (e.g. `"Radiohead/OK Computer"`).
    /// For tracks: local virtual cache path (e.g. `"/mnt/SDCARD/.../tracks/raw/id.flac"`).
    pub target_path: String,
}

#[derive(Default)]
struct DirNode {
    subdirs: HashMap<String, usize>,
    entries: Vec<VirtualEntry>,
}

#[derive(Default)]
pub struct VirtualFolderTree {
    nodes: Vec<DirNode>,
    path_to_node: HashMap<String, usize>,
}

impl VirtualFolderTree {
    pub fn new() -> Self {
        let mut tree = Self {
            nodes: vec![DirNode::default()],
            path_to_node: HashMap::new(),
        };
        tree.path_to_node.insert(String::new(), 0);
        tree
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    fn ensure_dir(&mut self, rel_dir: &str) -> usize {
        if rel_dir.is_empty() {
            return 0;
        }
        if let Some(&idx) = self.path_to_node.get(rel_dir) {
            return idx;
        }

        let mut cur_idx = 0usize;
        let mut accum = String::new();
        for seg in rel_dir.split('/').filter(|s| !s.is_empty()) {
            if !accum.is_empty() {
                accum.push('/');
            }
            accum.push_str(seg);

            if let Some(&next_idx) = self.nodes[cur_idx].subdirs.get(seg) {
                cur_idx = next_idx;
            } else {
                let new_idx = self.nodes.len();
                self.nodes.push(DirNode::default());
                self.nodes[cur_idx].subdirs.insert(seg.to_string(), new_idx);
                self.nodes[cur_idx].entries.push(VirtualEntry {
                    is_dir: true,
                    name: seg.to_string(),
                    target_path: accum.clone(),
                });
                self.path_to_node.insert(accum.clone(), new_idx);
                cur_idx = new_idx;
            }
        }
        cur_idx
    }

    pub fn insert_track(&mut self, server_path: &str, local_track_path: &str, fallback_title: &str) {
        let normalized = server_path.replace('\\', "/");
        let trimmed = normalized.trim_matches('/');

        let (parent_dir, leaf_name) = if trimmed.is_empty() {
            let leaf = if fallback_title.is_empty() {
                "Track"
            } else {
                fallback_title
            };
            ("", leaf)
        } else if let Some((dir, file)) = trimmed.rsplit_once('/') {
            (dir, if file.is_empty() { fallback_title } else { file })
        } else {
            ("", trimmed)
        };

        let dir_idx = self.ensure_dir(parent_dir);
        self.nodes[dir_idx].entries.push(VirtualEntry {
            is_dir: false,
            name: leaf_name.to_string(),
            target_path: local_track_path.to_string(),
        });
    }

    /// Sort all directory listings: folders first, then files, natural case-insensitive.
    pub fn finalize(&mut self) {
        for node in &mut self.nodes {
            node.entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                _ => natural_cmp_ci(&a.name, &b.name),
            });
        }
    }

    pub fn list(&self, rel_dir: &str) -> &[VirtualEntry] {
        let key = rel_dir.trim_matches('/');
        self.path_to_node
            .get(key)
            .and_then(|&idx| self.nodes.get(idx))
            .map(|n| n.entries.as_slice())
            .unwrap_or(&[])
    }

    /// Recursively collect all track `target_path`s under `rel_dir` in sorted tree order.
    pub fn collect_tracks(&self, rel_dir: &str, out: &mut Vec<String>) {
        let key = rel_dir.trim_matches('/');
        let Some(&idx) = self.path_to_node.get(key) else {
            return;
        };
        self.collect_from_node(idx, out);
    }

    fn collect_from_node(&self, node_idx: usize, out: &mut Vec<String>) {
        let Some(node) = self.nodes.get(node_idx) else {
            return;
        };
        for entry in &node.entries {
            if entry.is_dir {
                if let Some(&child_idx) = self.path_to_node.get(&entry.target_path) {
                    self.collect_from_node(child_idx, out);
                }
            } else {
                out.push(entry.target_path.clone());
            }
        }
    }
}

/// Natural case-insensitive comparison (`"2 - Song"` < `"10 - Song"`).
pub fn natural_cmp_ci(a: &str, b: &str) -> Ordering {
    let mut ca = a.chars().peekable();
    let mut cb = b.chars().peekable();

    loop {
        match (ca.peek().copied(), cb.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(xa), Some(xb)) if xa.is_ascii_digit() && xb.is_ascii_digit() => {
                // Compare numeric runs by value first (ignoring leading zeros), then length
                let mut za = 0usize;
                while ca.peek() == Some(&'0') {
                    ca.next();
                    za += 1;
                }
                let mut zb = 0usize;
                while cb.peek() == Some(&'0') {
                    cb.next();
                    zb += 1;
                }

                let mut da = Vec::new();
                while let Some(&d) = ca.peek() {
                    if d.is_ascii_digit() {
                        da.push(d as u8);
                        ca.next();
                    } else {
                        break;
                    }
                }
                let mut db = Vec::new();
                while let Some(&d) = cb.peek() {
                    if d.is_ascii_digit() {
                        db.push(d as u8);
                        cb.next();
                    } else {
                        break;
                    }
                }

                match da.len().cmp(&db.len()) {
                    Ordering::Equal => match da.cmp(&db) {
                        Ordering::Equal => {
                            if za != zb {
                                // Fewer leading zeros sorts after or before consistently
                                return zb.cmp(&za);
                            }
                        }
                        ord => return ord,
                    },
                    ord => return ord,
                }
            }
            (Some(_), Some(_)) => {
                let xa = ca.next().unwrap();
                let xb = cb.next().unwrap();
                let la = xa.to_lowercase().next().unwrap_or(xa);
                let lb = xb.to_lowercase().next().unwrap_or(xb);
                match la.cmp(&lb) {
                    Ordering::Equal => {}
                    ord => return ord,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_tree_hierarchy_and_natural_sort() {
        let mut tree = VirtualFolderTree::new();
        tree.insert_track(
            "Radiohead/OK Computer/10 - No Surprises.flac",
            "/cache/10.flac",
            "No Surprises",
        );
        tree.insert_track(
            "Radiohead/OK Computer/2 - Paranoid Android.flac",
            "/cache/2.flac",
            "Paranoid Android",
        );
        tree.insert_track(
            "Radiohead/Kid A/01 - Everything In Its Right Place.flac",
            "/cache/k1.flac",
            "Everything",
        );
        tree.finalize();

        let root = tree.list("");
        assert_eq!(root.len(), 1);
        assert!(root[0].is_dir);
        assert_eq!(root[0].name, "Radiohead");

        let albums = tree.list("Radiohead");
        assert_eq!(albums.len(), 2);
        assert_eq!(albums[0].name, "Kid A");
        assert_eq!(albums[1].name, "OK Computer");

        let tracks = tree.list("Radiohead/OK Computer");
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].name, "2 - Paranoid Android.flac");
        assert_eq!(tracks[1].name, "10 - No Surprises.flac");

        let mut all = Vec::new();
        tree.collect_tracks("Radiohead", &mut all);
        assert_eq!(all, vec!["/cache/k1.flac", "/cache/2.flac", "/cache/10.flac"]);
    }
}
