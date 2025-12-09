use ropey::Rope;
use tree_sitter::Tree;

pub struct Document {
    pub text: Rope,
    pub tree: Tree,
}

pub struct GlobalState {
    // TODO: index store
}
