use std::{
    any::{Any, TypeId, type_name},
    cell::{Ref, RefCell, RefMut},
    collections::HashMap,
};

use crate::hashed::Hashed;
pub struct World {
    pub table: HashMap<u128, u32>,
    pub storage: Vec<Box<RefCell<dyn Any>>>,
}

impl World {
    pub fn new() -> Self {
        Self {
            table: HashMap::new(),
            storage: Vec::new(),
        }
    }

    pub fn insert_resource<R: 'static + Hashed>(&mut self, resource: R) {
        let id = R::TYPE_UUID;
        if let Some(&index) = self.table.get(&id) {
            self.storage[index as usize] = Box::new(RefCell::new(resource));
        } else {
            self.table.insert(R::TYPE_UUID, self.storage.len() as u32);
            self.storage.push(Box::new(RefCell::new(resource)));
        }
    }

    pub fn init_resource<R: 'static + Default + Hashed>(&mut self) {
        self.insert_resource(R::default());
    }

    pub fn remove_resource<R: 'static + Default + Hashed>(&mut self) -> bool {
        let id = R::TYPE_UUID;
        if let Some(index) = self.table.remove(&id) {
            self.storage.swap_remove(index as usize);
            return true
        }
        false
    }

    pub fn has_resource<R: 'static + Hashed>(&self) -> bool {
        let id = R::TYPE_UUID;
        self.table.contains_key(&id)
    }

    pub fn get_resource<R: 'static + Hashed>(&self) -> Option<Ref<'_, R>> {
        let id = R::TYPE_UUID;
        let index = *self.table.get(&id)?;
        let storage_cell = self.storage.get(index as usize)?;
        let borrow = storage_cell.borrow();
        Ref::filter_map(borrow, |any| any.downcast_ref::<R>()).ok()
    }

    pub fn get_resource_mut<R: 'static + Hashed>(&mut self) -> Option<RefMut<'_, R>> {
        self.get_resource_mut_silent::<R>()
    }

    pub fn get_resource_mut_silent<R: 'static + Hashed>(&self) -> Option<RefMut<'_, R>> {
        let id = R::TYPE_UUID;
        let index = *self.table.get(&id)?;
        let storage_cell = self.storage.get(index as usize)?;

        let borrow = storage_cell.borrow_mut();
        RefMut::filter_map(borrow, |any| any.downcast_mut::<R>()).ok()
    }
}

pub struct StoredSerializer {
    pub serialize_fn: Box<dyn FnMut(&World)>,
}
