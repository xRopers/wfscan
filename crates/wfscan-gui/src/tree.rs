//! The files as a tree of folders: packages at the top, then each path's folders. No
//! drawing code. The tree is built from the files the filter lets through, and flattened
//! into the rows on screen from the folders that are open.

use std::collections::HashSet;

use wfscan_core::FileEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub name: String,
    /// The package and folders down to this one, such as `Misc/Lotus/Sounds`.
    pub key: String,
    pub parent: Option<usize>,
    /// Folders inside this one, as positions in [`Tree::folders`].
    pub folders: Vec<usize>,
    /// Files directly inside, as positions in the session's file list.
    pub files: Vec<usize>,
    /// Files inside at any depth.
    pub total: usize,
}

/// One line of the tree on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Folder { folder: usize, depth: usize },
    File { file: usize, depth: usize },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    /// Every folder, each before the folders inside it.
    pub folders: Vec<Folder>,
    /// The packages.
    pub top: Vec<usize>,
    /// Per entry of the file list: the folder it is in, if the tree holds it.
    folder_of: Vec<Option<u32>>,
}

/// A file's name: the last part of its path.
pub fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl Tree {
    /// The tree of the files at `view` (positions in `files`).
    pub fn build(files: &[FileEntry], view: &[usize]) -> Self {
        // In this order everything inside a folder is together.
        let mut order = view.to_vec();
        order.sort_by(|&a, &b| (&files[a].package, &files[a].path).cmp(&(&files[b].package, &files[b].path)));

        let mut tree = Self { folders: Vec::new(), top: Vec::new(), folder_of: vec![None; files.len()] };
        // The folders the previous file was in, outermost first.
        let mut open: Vec<usize> = Vec::new();
        for index in order {
            let file = &files[index];
            let mut parts: Vec<&str> = file.path.split('/').filter(|p| !p.is_empty()).collect();
            parts.pop();
            let names = std::iter::once(file.package.as_str()).chain(parts);
            let mut depth = 0;
            for name in names {
                if open.get(depth).is_none_or(|&f| tree.folders[f].name != name) {
                    open.truncate(depth);
                    let parent = open.last().copied();
                    let key = match parent {
                        Some(p) => format!("{}/{name}", tree.folders[p].key),
                        None => name.to_string(),
                    };
                    let id = tree.folders.len();
                    tree.folders.push(Folder { name: name.to_string(), key, parent, folders: Vec::new(), files: Vec::new(), total: 0 });
                    match parent {
                        Some(p) => tree.folders[p].folders.push(id),
                        None => tree.top.push(id),
                    }
                    open.push(id);
                }
                depth += 1;
            }
            open.truncate(depth);
            let folder = *open.last().expect("every file is in its package");
            tree.folders[folder].files.push(index);
            tree.folder_of[index] = Some(folder as u32);
        }
        // Folders come before what they contain, so going backwards totals add up.
        for id in (0..tree.folders.len()).rev() {
            tree.folders[id].total += tree.folders[id].files.len();
            if let Some(parent) = tree.folders[id].parent {
                tree.folders[parent].total += tree.folders[id].total;
            }
        }
        tree
    }

    /// The lines to show: every package, and the contents of each folder `is_open` says
    /// is open, folders before files.
    pub fn rows(&self, is_open: impl Fn(&Folder) -> bool) -> Vec<Row> {
        let mut rows = Vec::new();
        // Folders still to show, last first.
        let mut pending: Vec<(usize, usize)> = self.top.iter().rev().map(|&f| (f, 0)).collect();
        loop {
            self.take_files(&mut pending, &mut rows);
            let Some((id, depth)) = pending.pop() else { break };
            let folder = &self.folders[id];
            rows.push(Row::Folder { folder: id, depth });
            if !is_open(folder) {
                continue;
            }
            // Files go after the last line of the last folder inside, so they wait on
            // the same stack, marked by a folder number past the end.
            if !folder.files.is_empty() {
                pending.push((self.folders.len() + id, depth + 1));
            }
            pending.extend(folder.folders.iter().rev().map(|&f| (f, depth + 1)));
        }
        rows
    }

    /// Turn the file markers at the top of the stack into rows.
    fn take_files(&self, pending: &mut Vec<(usize, usize)>, rows: &mut Vec<Row>) {
        while let Some(&(marker, depth)) = pending.last().filter(|(id, _)| *id >= self.folders.len()) {
            pending.pop();
            rows.extend(self.folders[marker - self.folders.len()].files.iter().map(|&file| Row::File { file, depth }));
        }
    }

    /// The folders `file` is inside, innermost first.
    pub fn ancestors(&self, file: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut next = self.folder_of.get(file).copied().flatten().map(|f| f as usize);
        while let Some(folder) = next {
            out.push(folder);
            next = self.folders[folder].parent;
        }
        out
    }
}

/// Which folders are open: all or none by default, and the ones the user changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenFolders {
    /// Whether a folder nobody clicked is open.
    pub by_default: bool,
    /// Keys of the folders that are the other way.
    changed: HashSet<String>,
}

impl OpenFolders {
    pub fn is_open(&self, folder: &Folder) -> bool {
        self.by_default != self.changed.contains(&folder.key)
    }

    pub fn set(&mut self, folder: &Folder, open: bool) {
        if open == self.by_default {
            self.changed.remove(&folder.key);
        } else {
            self.changed.insert(folder.key.clone());
        }
    }

    pub fn toggle(&mut self, folder: &Folder) {
        self.set(folder, !self.is_open(folder));
    }

    /// Open or close every folder.
    pub fn set_all(&mut self, open: bool) {
        self.by_default = open;
        self.changed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[(&str, &str)]) -> Vec<FileEntry> {
        list.iter()
            .map(|(package, path)| FileEntry {
                package: package.to_string(),
                path: path.to_string(),
                stale: false,
                h: None,
                b: None,
                f: None,
                header: None,
                error: None,
                export: None,
            })
            .collect()
    }

    fn lines(tree: &Tree, files: &[FileEntry], open: &OpenFolders) -> Vec<String> {
        tree.rows(|f| open.is_open(f))
            .iter()
            .map(|row| match *row {
                Row::Folder { folder, depth } => format!("{}{}/ ({})", "  ".repeat(depth), tree.folders[folder].name, tree.folders[folder].total),
                Row::File { file, depth } => format!("{}{}", "  ".repeat(depth), file_name(&files[file].path)),
            })
            .collect()
    }

    #[test]
    fn folders_then_files_at_every_level() {
        let files = files(&[
            ("Misc", "/Lotus/B.x"),
            ("Misc", "/Lotus/Sounds/Tone.wav"),
            ("Misc", "/Lotus/A.x"),
            ("Texture", "/Lotus/Sounds.png"),
            ("Misc", "/Lotus/Sounds/Deep/Low.wav"),
            ("Misc", "/EE/One"),
            ("Misc", "/Top"),
        ]);
        let tree = Tree::build(&files, &[0, 1, 2, 3, 4, 5, 6]);
        let mut open = OpenFolders::default();
        assert_eq!(lines(&tree, &files, &open), ["Misc/ (6)", "Texture/ (1)"]);

        open.set_all(true);
        #[rustfmt::skip]
        let all = [
            "Misc/ (6)",
            "  EE/ (1)", "    One",
            "  Lotus/ (4)",
            "    Sounds/ (2)", "      Deep/ (1)", "        Low.wav", "      Tone.wav",
            "    A.x", "    B.x",
            "  Top",
            "Texture/ (1)", "  Lotus/ (1)", "    Sounds.png",
        ];
        assert_eq!(lines(&tree, &files, &open), all);

        // Close one folder; the rest stay open.
        let sounds = tree.folders.iter().find(|f| f.key == "Misc/Lotus/Sounds").unwrap();
        open.toggle(sounds);
        assert!(!open.is_open(sounds));
        let closed = lines(&tree, &files, &open);
        assert_eq!(&closed[3..7], ["  Lotus/ (4)", "    Sounds/ (2)", "    A.x", "    B.x"]);

        // With everything closed, opening a file's folders shows it.
        open.set_all(false);
        for folder in tree.ancestors(4) {
            open.set(&tree.folders[folder], true);
        }
        assert_eq!(lines(&tree, &files, &open), ["Misc/ (6)", "  EE/ (1)", "  Lotus/ (4)", "    Sounds/ (2)", "      Deep/ (1)", "        Low.wav", "      Tone.wav", "    A.x", "    B.x", "  Top", "Texture/ (1)"]);
    }

    #[test]
    fn only_the_files_in_view_are_in_the_tree() {
        let files = files(&[("Misc", "/Lotus/A.x"), ("Misc", "/Lotus/Sounds/Tone.wav"), ("Texture", "/Lotus/T.png")]);
        let tree = Tree::build(&files, &[1]);
        let open = OpenFolders { by_default: true, ..Default::default() };
        assert_eq!(lines(&tree, &files, &open), ["Misc/ (1)", "  Lotus/ (1)", "    Sounds/ (1)", "      Tone.wav"]);
        assert!(tree.ancestors(0).is_empty() && tree.ancestors(99).is_empty());
        assert_eq!(tree.ancestors(1).len(), 3);
        assert!(Tree::build(&files, &[]).rows(|_| true).is_empty());
    }
}
