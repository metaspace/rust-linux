use core::{array::IntoIter, marker::PhantomData};
use core::ptr::addr_of_mut;
use init::PinnedDrop;
use kernel::alloc::flags;

use crate::{prelude::*, types::Opaque};

#[pin_data]
pub struct Subsystem<A, G>
{
    #[pin]
    subsystem: Opaque<bindings::configfs_subsystem>,
    _p: PhantomData<(A, G)>,
}

unsafe impl<A, G> Sync for Subsystem<A,G>
{}

unsafe impl<A, G> Send for Subsystem<A,G>
{}

impl<A, G> Subsystem<A, G>
where
    A: AttributeOperations,
    G: GroupOperations,
{
    pub fn new(
        name: &'static CStr,
        owner: &ThisModule,
        tpe: &'static ItemType<A, G>,
    ) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            subsystem <- Opaque::try_ffi_init(|place: *mut bindings::configfs_subsystem| {
                unsafe {addr_of_mut!((*place).su_group.cg_item.ci_name ).write(name.as_ptr() as _) };
                unsafe {addr_of_mut!((*place).su_group.cg_item.ci_type).write(tpe.as_ptr()) };
                unsafe { bindings::config_group_init(&mut (*place).su_group) };
                crate::error::to_result( unsafe {bindings::configfs_register_subsystem(place)} )
            }),
            _p: PhantomData,
        })
    }
}

#[pin_data]
struct Group {
    #[pin]
    group: Opaque<bindings::config_group>,
}

impl Group {
    pub fn new() -> impl PinInit<Self> {
        pin_init!(Self {
            group <- Opaque::ffi_init(|place: *mut bindings::config_group| {
                unsafe { bindings::config_group_init(place) }
            }),
        })
    }
}

struct GroupOperationsVTable<T: GroupOperations>(PhantomData<T>);

impl<T> GroupOperationsVTable<T>
where
    T: GroupOperations,
{
    unsafe extern "C" fn make_group(
        group: *mut bindings::config_group,
        name: *const kernel::ffi::c_char,
    ) -> *mut bindings::config_group {
        todo!()
    }

    unsafe extern "C" fn drop_item(
        group: *mut bindings::config_group,
        item: *mut bindings::config_item,
    ) {
        todo!()
    }

    const VTABLE: bindings::configfs_group_operations = bindings::configfs_group_operations {
        make_item: None,
        make_group: Some(Self::make_group),
        disconnect_notify: None,
        drop_item: Some(Self::drop_item),
        is_visible: None,
        is_bin_visible: None,
    };
}

pub trait GroupOperations {
    fn make_group();
    fn drop_item();
}

#[repr(transparent)]
pub struct Attribute<T> {
    attribute: Opaque<bindings::configfs_attribute>,
    _p: PhantomData<T>,
}

unsafe impl<A> Sync for Attribute<A>
{}

unsafe impl<A> Send for Attribute<A>
{}

impl<T> Attribute<T>
where
    T: AttributeOperations,
{
    unsafe extern "C" fn show(item: *mut bindings::config_item, page: *mut kernel::ffi::c_char) -> isize{
        T::show(unsafe {&mut *(page as *mut [u8; 4096])})
    }

    unsafe extern "C" fn store(
        item: *mut bindings::config_item,
        page: *const kernel::ffi::c_char,
        size: usize,
    ) -> isize {
        T::store(unsafe {core::slice::from_raw_parts(page.cast(), size)})
    }

    pub const fn new(name: &'static CStr) -> Self {
        Self {
            attribute: unsafe { Opaque::new(bindings::configfs_attribute {
                ca_name: name as *const _ as _,
                ca_owner: core::ptr::null_mut(),
                ca_mode: 0o660,
                show: Some(Self::show),
                store: None,
            })},
            _p: PhantomData,
        }
    }
}

pub trait AttributeOperations {
    fn show(page: &mut [u8; 4096]) -> isize;
    fn store(page: &[u8]) -> isize;
}

#[repr(transparent)]
pub struct AttributeList<const N: usize>(pub [*mut kernel::ffi::c_void; N]);
unsafe impl<const N: usize> Send for AttributeList<N> {}
unsafe impl<const N: usize> Sync for AttributeList<N> {}

#[pin_data]
pub struct ItemType<A, G>
{
    #[pin]
    item_type: Opaque<bindings::config_item_type>,
    _p: PhantomData<(A,G)>,
}


unsafe impl<A, G> Sync for ItemType<A,G>
{}

unsafe impl<A, G> Send for ItemType<A,G>
{}

impl<A, G> ItemType<A, G>
where
    A: AttributeOperations,
    G: GroupOperations,
{
    pub const fn new<const N: usize>(attributes: &'static AttributeList<N>) -> Self {
        Self {
            item_type: Opaque::new(bindings::config_item_type {
                ct_owner: core::ptr::null_mut(),
                ct_group_ops: (&GroupOperationsVTable::<G>::VTABLE as *const _) as *mut _ ,
                ct_item_ops: core::ptr::null_mut(),
                ct_attrs: attributes as *const _ as _,
                ct_bin_attrs: core::ptr::null_mut(),
            }),
            _p: PhantomData,
        }
    }

    fn as_ptr(&self) -> *const bindings::config_item_type {
        self.item_type.get()
    }
}

// #[pin_data]
// struct Item {
//     #[pin]
//     item: Opaque<bindings::config_item>,
// }

// impl Item {
//     fn new(name: &str) -> impl PinInit<Self> {
//         pin_init!(Self {
//             item <- Opaque::ffi_init(|place| {
//                 todo!()
//             })
//         })
//     }
// }
