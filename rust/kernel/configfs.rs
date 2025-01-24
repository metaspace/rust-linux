use core::cell::UnsafeCell;
use core::ptr::addr_of_mut;
use core::{array::IntoIter, marker::PhantomData};
use init::PinnedDrop;
use kernel::alloc::flags;
use kernel::str::CString;

use crate::types::ForeignOwnable;
use crate::{prelude::*, types::Opaque};

#[pin_data]
pub struct Subsystem<C> {
    #[pin]
    subsystem: Opaque<bindings::configfs_subsystem>,
    _p: PhantomData<C>,
}

unsafe impl<C> Sync for Subsystem<C> {}
unsafe impl<C> Send for Subsystem<C> {}

impl<C> Subsystem<C> {
    pub fn new(
        name: &'static CStr,
        owner: &ThisModule,
        tpe: &'static ItemType<C>,
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
#[repr(transparent)]
pub struct Group<C> {
    #[pin]
    group: Opaque<bindings::config_group>,
    _p: PhantomData<C>,
}

impl<C> Group<C>
where
    C: 'static,
{
    pub fn new(name: CString, tpe: &'static ItemType<C>) -> impl PinInit<Self> {
        pin_init!(Self {
            group <- kernel::init::zeroed().chain(|v: &mut Opaque<bindings::config_group>| {
                let place = v.get();
                let name = name.as_bytes_with_nul().as_ptr();
                unsafe { bindings::config_group_init_type_name(place, name as _, tpe.as_ptr()) }
                Ok(())
            }),
            _p: PhantomData,
        })
    }
}

struct GroupOperationsVTable<PAR, CHLD>(PhantomData<(PAR, CHLD)>)
where
    PAR: GroupOperations<PAR, CHLD> + HasGroup,
    CHLD: HasGroup;

impl<PAR, CHLD> GroupOperationsVTable<PAR, CHLD>
where
    PAR: GroupOperations<PAR, CHLD> + HasGroup,
    CHLD: HasGroup + 'static,
{
    unsafe extern "C" fn make_group(
        parent_group: *mut bindings::config_group,
        name: *const kernel::ffi::c_char,
    ) -> *mut bindings::config_group {
        let r_group_ptr: *mut Group<PAR> = parent_group.cast();
        let container_ptr = unsafe { PAR::container_ptr(r_group_ptr) };
        let container_ref = unsafe { &*container_ptr };
        let child = PAR::make_group(container_ref, unsafe { CStr::from_char_ptr(name) });

        match child {
            Ok(child) => {
                let child_ptr = child.into_foreign();
                unsafe { CHLD::group_ptr(child_ptr) }
                    .cast::<bindings::config_group>()
                    .cast_mut()
            }
            Err(e) => e.to_ptr(),
        }
    }

    unsafe extern "C" fn drop_item(
        parent_group: *mut bindings::config_group,
        item: *mut bindings::config_item,
    ) {
        let c_group_ptr = unsafe {kernel::container_of!(item, bindings::config_group, cg_item)};
        let r_group_ptr: *mut Group<PAR> = parent_group.cast();
        let container_ptr = unsafe { PAR::container_ptr(r_group_ptr) };
        let bx = KBox::from_foreign(container_ptr).;

        PAR::drop_item(container_ref);
        unsafe { bindings::config_item_put(item) };
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

pub trait GroupOperations<PAR, CHLD>
where
    PAR: HasGroup,
    CHLD: HasGroup,
{
    // TODO: Should probably be an Arc or Pin<Deref<Target = CHLD>>
    fn make_group(container: &PAR, name: &CStr) -> Result<Pin<KBox<CHLD>>>;
    fn drop_item(container: &PAR);
}

#[repr(C)]
pub struct Attribute<AO, HG> {
    attribute: Opaque<bindings::configfs_attribute>,
    _p: PhantomData<(AO, HG)>,
}

unsafe impl<AO, HG> Sync for Attribute<AO, HG> {}

unsafe impl<AO, HG> Send for Attribute<AO, HG> {}

impl<AO, HG> Attribute<AO, HG>
where
    AO: AttributeOperations<HG>,
    HG: HasGroup,
{
    unsafe extern "C" fn show(
        item: *mut bindings::config_item,
        page: *mut kernel::ffi::c_char,
    ) -> isize {
        let c_group: *mut bindings::config_group = item.cast();
        let r_group_ptr: *mut Group<HG> = c_group.cast();
        let container_ptr = unsafe { HG::container_ptr(r_group_ptr) };
        let container_ref = unsafe { &*container_ptr };
        AO::show(container_ref, unsafe { &mut *(page as *mut [u8; 4096]) })
    }

    unsafe extern "C" fn store(
        item: *mut bindings::config_item,
        page: *const kernel::ffi::c_char,
        size: usize,
    ) -> isize {
        let c_group: *mut bindings::config_group = item.cast();
        let r_group_ptr: *mut Group<HG> = c_group.cast();
        let container_ptr = unsafe { HG::container_ptr(r_group_ptr) };
        let container_ref = unsafe { &*container_ptr };
        AO::store(container_ref, unsafe {
            core::slice::from_raw_parts(page.cast(), size)
        })
    }

    pub const fn new(name: &'static CStr) -> Self {
        Self {
            attribute: unsafe {
                Opaque::new(bindings::configfs_attribute {
                    ca_name: name as *const _ as _,
                    ca_owner: core::ptr::null_mut(),
                    ca_mode: 0o660,
                    show: Some(Self::show),
                    store: Some(Self::store),
                })
            },
            _p: PhantomData,
        }
    }
}

pub trait AttributeOperations<AO>
where
    AO: HasGroup,
{
    fn show(container: &AO, page: &mut [u8; 4096]) -> isize;
    fn store(container: &AO, page: &[u8]) -> isize;
}

#[repr(transparent)]
pub struct AttributeList<const N: usize, C>(
    UnsafeCell<[*mut kernel::ffi::c_void; N]>,
    PhantomData<C>,
)
where
    C: HasGroup;
unsafe impl<const N: usize, C: HasGroup> Send for AttributeList<N, C> {}
unsafe impl<const N: usize, C: HasGroup> Sync for AttributeList<N, C> {}

impl<const N: usize, C: HasGroup> AttributeList<N, C> {
    pub const fn new() -> Self {
        Self(UnsafeCell::new([core::ptr::null_mut(); N]), PhantomData)
    }

    pub const fn add<const I: usize, O: AttributeOperations<C>>(
        &'static self,
        attribute: &'static Attribute<O, C>,
    ) {
        // TODO: bound check for null terminator
        unsafe { (&mut *self.0.get())[I] = attribute as *const _ as _ };
    }
}

#[pin_data]
pub struct ItemType<C> {
    #[pin]
    item_type: Opaque<bindings::config_item_type>,
    _p: PhantomData<C>,
}

unsafe impl<C> Sync for ItemType<C> {}
unsafe impl<C> Send for ItemType<C> {}

impl<C: HasGroup> ItemType<C> {
    pub const fn new<const N: usize, PAR, CHLD>(attributes: &'static AttributeList<N, C>) -> Self
    where
        PAR: GroupOperations<PAR, CHLD> + HasGroup,
        CHLD: HasGroup + 'static,
    {
        Self {
            item_type: Opaque::new(bindings::config_item_type {
                ct_owner: core::ptr::null_mut(),
                ct_group_ops: (&GroupOperationsVTable::<PAR, CHLD>::VTABLE as *const _) as *mut _,
                ct_item_ops: core::ptr::null_mut(),
                ct_attrs: attributes as *const _ as _,
                ct_bin_attrs: core::ptr::null_mut(),
            }),
            _p: PhantomData,
        }
    }

    pub const fn new2<const N: usize>(attributes: &'static AttributeList<N, C>) -> Self {
        Self {
            item_type: Opaque::new(bindings::config_item_type {
                ct_owner: core::ptr::null_mut(),
                ct_group_ops: core::ptr::null_mut(),
                ct_item_ops: core::ptr::null_mut(),
                ct_attrs: attributes as *const _ as _,
                ct_bin_attrs: core::ptr::null_mut(),
            }),
            _p: PhantomData,
        }
    }
}

impl<C> ItemType<C> {
    fn as_ptr(&self) -> *const bindings::config_item_type {
        self.item_type.get()
    }
}

pub unsafe trait HasGroup {
    const OFFSET: usize;
    unsafe fn group_ptr(self: *const Self) -> *const Group<Self>
    where
        Self: Sized,
    {
        unsafe { self.cast::<u8>().add(Self::OFFSET).cast::<Group<Self>>() }
    }

    unsafe fn container_ptr(group: *mut Group<Self>) -> *mut Self
    where
        Self: Sized,
    {
        unsafe { group.cast::<u8>().sub(Self::OFFSET).cast::<Self>() }
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
