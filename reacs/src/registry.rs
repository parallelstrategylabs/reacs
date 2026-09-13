use std::{any::Any, collections::HashMap, ptr::addr_of_mut};

pub struct Entry {
    pub serialize: fn(&dyn Any) -> Vec<u8>,
    pub deserialize: fn(&[u8]) -> Box<dyn Any>,
}

static mut REGISTRY: Option<HashMap<u128, Entry>> = None;

pub fn register_entry<T>(id: u128)
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + 'static,
{
    unsafe {
        let ptr = addr_of_mut!(REGISTRY);
        let map = (*ptr).get_or_insert_with(HashMap::new);
        map.insert(
            id,
            Entry {
                serialize: |v| bincode::serialize(v.downcast_ref::<T>().unwrap()).unwrap(),
                deserialize: |b| Box::new(bincode::deserialize::<T>(b).unwrap()),
            },
        );
    }
}

pub fn get_registry_entry(id: u128) -> Option<&'static Entry> {
    unsafe {
        let ptr = addr_of_mut!(REGISTRY);
        (*ptr).as_ref()?.get(&id)
    }
}
