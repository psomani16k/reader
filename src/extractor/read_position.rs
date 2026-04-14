use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone)]
pub struct ReadPosition {
    pub file_name: String,
    pub node_path: Vec<usize>,
    pub offset: usize,
}

impl ReadPosition {
    pub fn new_default(file_name: String) -> Self {
        return ReadPosition {
            file_name,
            node_path: vec![],
            offset: 0,
        };
    }
}

#[derive(Deserialize, Serialize, Default)]
pub struct ReadPositionFileData {
    pub read_position: HashMap<String, ReadPosition>,
}
