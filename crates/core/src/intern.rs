use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StringId(pub u32);

#[derive(Debug, Clone)]
pub struct StringInterner {
    values: Vec<String>,
    index: Option<HashMap<String, StringId>>,
}

#[derive(Debug, Default, Clone)]
pub struct StringTable {
    offsets: Vec<u32>,
    bytes: Vec<u8>,
}

impl StringInterner {
    #[must_use]
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            index: Some(HashMap::new()),
        }
    }

    #[must_use]
    pub fn without_lookup() -> Self {
        Self {
            values: Vec::new(),
            index: None,
        }
    }

    pub fn intern(&mut self, value: &str) -> StringId {
        if let Some(index) = &mut self.index {
            if let Some(id) = index.get(value) {
                return *id;
            }

            let id = StringId(self.values.len() as u32);
            let owned = value.to_owned();
            self.values.push(owned.clone());
            index.insert(owned, id);
            id
        } else {
            let id = StringId(self.values.len() as u32);
            self.values.push(value.to_owned());
            id
        }
    }

    pub fn intern_owned(&mut self, value: String) -> StringId {
        if let Some(index) = &mut self.index {
            if let Some(id) = index.get(value.as_str()) {
                return *id;
            }

            let id = StringId(self.values.len() as u32);
            self.values.push(value.clone());
            index.insert(value, id);
            id
        } else {
            let id = StringId(self.values.len() as u32);
            self.values.push(value);
            id
        }
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
        let byte_len = self.values.iter().map(String::len).sum();
        let mut offsets = Vec::with_capacity(self.values.len().saturating_add(1));
        let mut bytes = Vec::with_capacity(byte_len);
        offsets.push(0);
        for value in self.values {
            bytes.extend_from_slice(value.as_bytes());
            offsets.push(bytes.len() as u32);
        }
        StringTable { offsets, bytes }
    }
}

impl Default for StringInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl StringTable {
    #[must_use]
    pub fn get(&self, id: StringId) -> Option<&str> {
        let idx = id.0 as usize;
        let start = *self.offsets.get(idx)? as usize;
        let end = *self.offsets.get(idx.saturating_add(1))? as usize;
        std::str::from_utf8(self.bytes.get(start..end)?).ok()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
    fn without_lookup_should_append_duplicate_strings() {
        let mut interner = StringInterner::without_lookup();

        let first = interner.intern("readme.md");
        let second = interner.intern("readme.md");

        assert_ne!(first, second);
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn intern_owned_without_lookup_should_append_string() {
        let mut interner = StringInterner::without_lookup();

        let id = interner.intern_owned("readme.md".to_owned());

        assert_eq!(interner.get(id), Some("readme.md"));
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

    #[test]
    fn finish_should_pack_names_without_per_name_string_headers() {
        let mut interner = StringInterner::without_lookup();
        let first = interner.intern("Program Files");
        let second = interner.intern("readme.md");

        let table = interner.finish();

        assert_eq!(table.len(), 2);
        assert_eq!(table.get(first), Some("Program Files"));
        assert_eq!(table.get(second), Some("readme.md"));
    }
}
