use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StringId(pub u32);

#[derive(Debug, Default, Clone)]
pub struct StringInterner {
    values: Vec<String>,
    index: HashMap<String, StringId>,
}

#[derive(Debug, Default, Clone)]
pub struct StringTable {
    values: Vec<String>,
}

impl StringInterner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, value: &str) -> StringId {
        if let Some(id) = self.index.get(value) {
            return *id;
        }

        let id = StringId(self.values.len() as u32);
        let owned = value.to_owned();
        self.values.push(owned.clone());
        self.index.insert(owned, id);
        id
    }

    #[must_use]
    pub fn get(&self, id: StringId) -> Option<&str> {
        self.values.get(id.0 as usize).map(String::as_str)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    #[must_use]
    pub fn finish(self) -> StringTable {
        StringTable {
            values: self.values,
        }
    }
}

impl StringTable {
    #[must_use]
    pub fn get(&self, id: StringId) -> Option<&str> {
        self.values.get(id.0 as usize).map(String::as_str)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::StringInterner;

    #[test]
    fn intern_should_reuse_existing_id_for_same_string() {
        let mut interner = StringInterner::new();

        let first = interner.intern("readme.md");
        let second = interner.intern("readme.md");

        assert_eq!(first, second);
    }

    #[test]
    fn get_should_return_interned_string() {
        let mut interner = StringInterner::new();
        let id = interner.intern("src");

        assert_eq!(interner.get(id), Some("src"));
    }

    #[test]
    fn finish_should_drop_lookup_index_and_keep_values() {
        let mut interner = StringInterner::new();
        let id = interner.intern("src");

        let table = interner.finish();

        assert_eq!(table.get(id), Some("src"));
    }
}
