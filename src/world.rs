use std::{any::{Any, TypeId}, cell::{RefCell, UnsafeCell}, collections::HashMap};

pub struct World {
    pub storage: HashMap<TypeId, Box<RefCell<dyn Any>>>,
}

impl World {
    pub fn new() -> Self {
        Self { storage: HashMap::new() }
    }

    pub fn register_resource<R: >(&mut self, resource: R) {
        
    }
}

pub struct StoredSerializer {
    pub serialize_fn: Box<dyn FnMut(&World)>,
}