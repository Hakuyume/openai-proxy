pub struct Dedup<T> {
    value: Option<T>,
}

impl<T> Dedup<T> {
    pub fn update(&mut self, value: T) -> Option<&T>
    where
        T: PartialEq,
    {
        let is_equal = self.value.as_ref() == Some(&value);
        let value = self.value.insert(value);
        (!is_equal).then_some(value)
    }
}

impl<T> Default for Dedup<T> {
    fn default() -> Self {
        Self { value: None }
    }
}
