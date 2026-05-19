// In-memory hierarchical filesystem.
//
// Pure RAM, no disk, no inodes in the Unix sense — every "file" is just a
// `Vec<u8>` and every "directory" is a `BTreeMap<String, Node>`. This is the
// minimum required to make `ls`, `cat`, `mkdir`, `touch`, `rm`, `pwd` and
// `cd` feel real. When we ever add an ATA or virtio-blk driver it can plug
// in beneath this same `Fs` API by adding a `FilesystemBackend` trait.
//
// Concurrency model: a single global `Mutex<Fs>` because all access happens
// from the shell task (and any later helper tasks).

use alloc::{
    borrow::ToOwned,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};
use lazy_static::lazy_static;
use spin::Mutex;

#[derive(Debug, Clone)]
pub enum Node {
    File(Vec<u8>),
    Dir(BTreeMap<String, Node>),
}

impl Node {
    pub fn new_dir() -> Self {
        Node::Dir(BTreeMap::new())
    }

    pub fn new_file(contents: &[u8]) -> Self {
        Node::File(contents.to_owned())
    }

    pub fn is_dir(&self) -> bool {
        matches!(self, Node::Dir(_))
    }
}

#[derive(Debug)]
pub enum FsError {
    NotFound,
    NotADirectory,
    NotAFile,
    AlreadyExists,
    DirectoryNotEmpty,
    InvalidPath,
    EmptyName,
}

impl FsError {
    pub fn description(&self) -> &'static str {
        match self {
            FsError::NotFound => "no such file or directory",
            FsError::NotADirectory => "not a directory",
            FsError::NotAFile => "is a directory",
            FsError::AlreadyExists => "already exists",
            FsError::DirectoryNotEmpty => "directory not empty (use -r)",
            FsError::InvalidPath => "invalid path",
            FsError::EmptyName => "missing operand",
        }
    }
}

pub struct Fs {
    root: Node,
    /// Path components of the working directory, never including "".
    cwd: Vec<String>,
}

impl Fs {
    fn new() -> Self {
        // Boot-time tree. Mimics the spirit of /etc/hostname and friends so
        // the shell has something interesting to `cat` on first launch.
        let mut root = BTreeMap::new();
        let mut etc = BTreeMap::new();
        etc.insert(
            "hostname".to_string(),
            Node::new_file(b"luxx-vm\n"),
        );
        etc.insert(
            "motd".to_string(),
            Node::new_file(b"Welcome to LUXX-OS in-memory VFS.\nRoot is /.\n"),
        );
        root.insert("etc".to_string(), Node::Dir(etc));
        root.insert("home".to_string(), Node::Dir(BTreeMap::new()));
        root.insert("tmp".to_string(), Node::Dir(BTreeMap::new()));

        Fs {
            root: Node::Dir(root),
            cwd: Vec::new(),
        }
    }

    pub fn pwd(&self) -> String {
        if self.cwd.is_empty() {
            "/".to_string()
        } else {
            let mut s = String::new();
            for c in &self.cwd {
                s.push('/');
                s.push_str(c);
            }
            s
        }
    }

    /// Turn a textual path into a canonical sequence of components, resolved
    /// against `cwd`. Handles `.`, `..`, leading `/`, repeated `/`.
    fn resolve_components(&self, path: &str) -> Result<Vec<String>, FsError> {
        let mut components: Vec<String> = if path.starts_with('/') {
            Vec::new()
        } else {
            self.cwd.clone()
        };
        for part in path.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    components.pop();
                }
                name => components.push(name.to_string()),
            }
        }
        Ok(components)
    }

    /// Walk the tree to the node identified by `components` and return an
    /// immutable reference to it. Empty `components` returns the root.
    fn lookup<'a>(&'a self, components: &[String]) -> Result<&'a Node, FsError> {
        let mut cur = &self.root;
        for c in components {
            let children = match cur {
                Node::Dir(map) => map,
                Node::File(_) => return Err(FsError::NotADirectory),
            };
            cur = children.get(c).ok_or(FsError::NotFound)?;
        }
        Ok(cur)
    }

    /// Same as `lookup` but yields a mutable reference. Cannot be merged with
    /// `lookup` because of lifetime variance on the BTreeMap accessors.
    fn lookup_mut<'a>(&'a mut self, components: &[String]) -> Result<&'a mut Node, FsError> {
        let mut cur = &mut self.root;
        for c in components {
            let children = match cur {
                Node::Dir(map) => map,
                Node::File(_) => return Err(FsError::NotADirectory),
            };
            cur = children.get_mut(c).ok_or(FsError::NotFound)?;
        }
        Ok(cur)
    }

    pub fn list(&self, path: &str) -> Result<Vec<(String, bool)>, FsError> {
        let comps = self.resolve_components(path)?;
        let node = self.lookup(&comps)?;
        match node {
            Node::Dir(children) => Ok(children
                .iter()
                .map(|(name, child)| (name.clone(), child.is_dir()))
                .collect()),
            Node::File(_) => {
                // Mirror `ls` behaviour on a regular file: report just the
                // file's basename.
                let name = comps.last().cloned().unwrap_or_default();
                Ok(alloc::vec![(name, false)])
            }
        }
    }

    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, FsError> {
        let comps = self.resolve_components(path)?;
        match self.lookup(&comps)? {
            Node::File(data) => Ok(data.clone()),
            Node::Dir(_) => Err(FsError::NotAFile),
        }
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), FsError> {
        let comps = self.resolve_components(path)?;
        if comps.is_empty() {
            return Err(FsError::AlreadyExists);
        }
        let (name, parent) = comps.split_last().unwrap();
        let parent_node = self.lookup_mut(parent)?;
        let map = match parent_node {
            Node::Dir(m) => m,
            Node::File(_) => return Err(FsError::NotADirectory),
        };
        if map.contains_key(name) {
            return Err(FsError::AlreadyExists);
        }
        map.insert(name.clone(), Node::new_dir());
        Ok(())
    }

    pub fn touch(&mut self, path: &str) -> Result<(), FsError> {
        self.write_file(path, b"")
    }

    pub fn write_file(&mut self, path: &str, contents: &[u8]) -> Result<(), FsError> {
        let comps = self.resolve_components(path)?;
        if comps.is_empty() {
            return Err(FsError::InvalidPath);
        }
        let (name, parent) = comps.split_last().unwrap();
        let parent_node = self.lookup_mut(parent)?;
        let map = match parent_node {
            Node::Dir(m) => m,
            Node::File(_) => return Err(FsError::NotADirectory),
        };
        // Replace existing file contents, but refuse to overwrite a directory.
        match map.get(name) {
            Some(Node::Dir(_)) => Err(FsError::AlreadyExists),
            _ => {
                map.insert(name.clone(), Node::new_file(contents));
                Ok(())
            }
        }
    }

    pub fn remove(&mut self, path: &str, recursive: bool) -> Result<(), FsError> {
        let comps = self.resolve_components(path)?;
        if comps.is_empty() {
            return Err(FsError::InvalidPath);
        }
        // Decide up-front whether we'll need to reset cwd, before taking the
        // mutable borrow of the parent directory.
        let reset_cwd = self.cwd.starts_with(&comps);
        let (name, parent) = comps.split_last().unwrap();
        let parent_node = self.lookup_mut(parent)?;
        let map = match parent_node {
            Node::Dir(m) => m,
            Node::File(_) => return Err(FsError::NotADirectory),
        };
        let target = map.get(name).ok_or(FsError::NotFound)?;
        if let Node::Dir(children) = target {
            if !children.is_empty() && !recursive {
                return Err(FsError::DirectoryNotEmpty);
            }
        }
        map.remove(name);
        if reset_cwd {
            // If we just `cd`'d into the doomed directory, fall back to root
            // so the user isn't left in a ghost cwd.
            self.cwd.clear();
        }
        Ok(())
    }

    pub fn chdir(&mut self, path: &str) -> Result<(), FsError> {
        let comps = self.resolve_components(path)?;
        let node = self.lookup(&comps)?;
        if !node.is_dir() {
            return Err(FsError::NotADirectory);
        }
        self.cwd = comps;
        Ok(())
    }
}

impl Default for Fs {
    fn default() -> Self {
        Self::new()
    }
}

lazy_static! {
    /// Singleton kernel VFS. Shell commands lock briefly per operation; no
    /// blocking calls held across yields, so the executor never deadlocks.
    pub static ref FS: Mutex<Fs> = Mutex::new(Fs::new());
}

/// Pretty-print a `ls`-style listing to a `String`. Directories suffixed with
/// `/`. Sorted because we walk a `BTreeMap` (already ordered) — included
/// explicitly to be obvious if we ever switch backing structures.
pub fn format_listing(entries: &[(String, bool)]) -> String {
    if entries.is_empty() {
        return String::from("(empty)\n");
    }
    let mut out = String::new();
    for (i, (name, is_dir)) in entries.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if *is_dir {
            out.push_str(&format!("{}/", name));
        } else {
            out.push_str(name);
        }
    }
    out.push('\n');
    out
}
