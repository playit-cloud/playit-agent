pub struct NonOverlapping<T> {
    elements: Vec<T>,
}

impl<T> NonOverlapping<T> {
    pub fn new() -> Self {
        NonOverlapping { elements: vec![] }
    }

    pub fn with(elem: T) -> Self {
        NonOverlapping {
            elements: vec![elem],
        }
    }

    pub fn add<C: NonOverlappingCheck<Element = T>>(
        &mut self,
        to_add: C::Element,
    ) -> Result<(), C::Element> {
        for item in &self.elements {
            if C::is_overlapping(&to_add, item) {
                return Err(to_add);
            }
        }

        self.elements.push(to_add);
        Ok(())
    }

    pub fn remove<C: NonOverlappingCheck<Element = T>>(&mut self, item: &C::Element) -> bool {
        let Some(pos) = self.elements.iter().position(|a| C::is_same(a, item)) else {
            return false;
        };

        self.elements.swap_remove(pos);
        true
    }

    pub fn contains<C: NonOverlappingCheck<Element = T>>(&self, item: &C::Element) -> bool {
        self.elements
            .iter()
            .position(|a| C::is_same(a, item))
            .is_some()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.elements.iter()
    }
}

pub trait NonOverlappingCheck {
    type Element;

    fn is_same(a: &Self::Element, b: &Self::Element) -> bool;

    fn is_overlapping(a: &Self::Element, b: &Self::Element) -> bool;
}
