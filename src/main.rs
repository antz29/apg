use std::{collections::HashMap, env, fs, io, path::PathBuf};

use tree_sitter::{Parser, Tree};

#[derive(Debug, Clone)]
struct Filemap {
    pub root: PathBuf,
    pub map: HashMap<PathBuf, Tree>,
}
impl Filemap {
    pub fn new(root: PathBuf) -> Self {
        Self { root, map: HashMap::new() }
    }
    pub fn populate(&mut self, parser: &mut Parser) -> Result<(), io::Error> {
        self.populate_inner(parser, self.root.clone())
    }
    /// will not follow symlinks
    fn populate_inner(&mut self, parser: &mut Parser, path: PathBuf) -> Result<(), io::Error> {
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let path = entry.path();

            if ty.is_symlink() {
                continue;
            }
            if ty.is_dir() {
                self.populate_inner(parser, path)?;
                continue;
            }
            if !ty.is_file() {
                // then what is it???
                unimplemented!()
            }

            if !entry.file_name().as_encoded_bytes().ends_with(b".java") {
                continue;
            }

            let text = fs::read(path.clone())?;
 
            // will only fail if no lang was set
            let tree = parser.parse(text, None).unwrap();

            self.map.insert(path, tree);
        }
        Ok(())
    }
}

fn main() {
    let mut parser = Parser::new();
    
    parser.set_language(&tree_sitter_java::LANGUAGE.into()).expect("Error loading java grammar");

    let mut filemap = Filemap::new(env::current_dir().unwrap().join("project"));
    filemap.populate(&mut parser).unwrap();

    dbg!(filemap);
}
