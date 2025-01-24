// SPDX-License-Identifier: GPL-2.0

//! Configfs interface.
//!
//! Features not covered:
//!
//! - Items. All group children are groups.
//! - Symlink support.
//! - `disconnect_notify` hook.
//! - Item `release` hook
//!

use core::cell::UnsafeCell;
use core::ops::Deref;
use core::ptr::addr_of_mut;
use core::ptr::addr_of;
use core::{array::IntoIter, marker::PhantomData};
use init::PinnedDrop;
use kernel::alloc::flags;
use kernel::str::CString;

use crate::types::ForeignOwnable;
use crate::{prelude::*, types::Opaque};

#[pin_data]
#[repr(transparent)]
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

    pub unsafe fn group_ptr(self: *const Self) -> *const Group<C> {
        let subsystem = self.cast::<bindings::configfs_subsystem>();
        unsafe {addr_of!((*subsystem).su_group)}.cast()
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
    PAR: GroupOperations<PAR, CHLD> + HasGroup + 'static,
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
        let r_group_ptr: *mut Group<PAR> = parent_group.cast();
        let container_ptr = unsafe { PAR::container_ptr(r_group_ptr) };
        let parent: &PAR = unsafe { KBox::<PAR>::borrow(container_ptr) };

        let c_group_ptr = unsafe { kernel::container_of!(item, bindings::config_group, cg_item) };
        let r_group_ptr: *mut Group<CHLD> = c_group_ptr.cast::<Group<CHLD>>().cast_mut();
        let container_ptr = unsafe { CHLD::container_ptr(r_group_ptr) };
        let child: KBox<CHLD> = unsafe { KBox::from_foreign(container_ptr) };

        PAR::drop_item(parent, child.deref());
        unsafe { bindings::config_item_put(item) };
        drop(child);
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
    /// Called by kernel to make a child node.
    fn make_group(container: &PAR, name: &CStr) -> Result<Pin<KBox<CHLD>>>;

    /// Called by kernel when a child node is about to be dropped.
    fn drop_item(this: &PAR, child: &CHLD);
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
    pub const fn new_with_child_ctor<const N: usize, PAR, CHLD>(attributes: &'static AttributeList<N, C>) -> Self
    where
        PAR: GroupOperations<PAR, CHLD> + HasGroup + 'static,
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

    pub const fn new<const N: usize>(attributes: &'static AttributeList<N, C>) -> Self {
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

/// Use to implement the [`HasGroup<T>`] trait for types that embed a [`Group`].
#[macro_export]
macro_rules! impl_has_group {
    (
        impl$({$($generics:tt)*})?
            HasGroup
            for $self:ty
        { self.$field:ident }
        $($rest:tt)*
    ) => {
        // SAFETY: This implementation of `group_ptr` only compiles if the
        // field has the right type.
        unsafe impl$(<$($generics)*>)? $crate::configfs::HasGroup for $self {
            const OFFSET: usize = ::core::mem::offset_of!(Self, $field) as usize;

            #[inline]
            unsafe fn group_ptr(self: *const Self) ->
                *const $crate::configfs::Group<Self>
            {
                // SAFETY: The caller promises that the pointer is not dangling.
                unsafe {
                    ::core::ptr::addr_of!((*self).$field)
                }
            }
        }
    }
}

/// Use to implement the [`HasGroup<T>`] trait for types that embed a [`Subsystem`].
#[macro_export]
macro_rules! impl_has_subsystem {
    (
        impl$({$($generics:tt)*})?
            HasGroup
            for $self:ty
        { self.$field:ident }
        $($rest:tt)*
    ) => {
        // SAFETY: This implementation of `group_ptr` only compiles if the
        // field has the right type.
        unsafe impl$(<$($generics)*>)? $crate::configfs::HasGroup for $self {
            const OFFSET: usize = ::core::mem::offset_of!(Self, $field) as usize;

            #[inline]
            unsafe fn group_ptr(self: *const Self) ->
                *const $crate::configfs::Group<Self>
            {

                // SAFETY: The caller promises that the pointer is not dangling.
                let subsystem: *const $crate::configfs::Subsystem<Self> = unsafe {
                    ::core::ptr::addr_of!((*self).$field)
                };

                // SAFETY: The caller promises that the pointer is not dangling.
                unsafe {
                    ::kernel::configfs::Subsystem::<Self>::group_ptr(subsystem)
                }
            }
        }
    }
}

macro_rules! count {
    () => (0usize);
    ($x:ident, $($xs:tt)* ) => (1usize + count!($($xs)*));
}

#[macro_export]
macro_rules! configfs_attrs {
    (
        container: $container:ty,
        attributes: [
            $($name:ident: $attr:ty,)+
        ],
    ) => {
        $crate::configfs_attrs!(
            count:
            @container($container),
            @child(),
            @no_child(x),
            @attrs($($name $attr)+),
            @eat($($name $attr,)+),
            @assign(),
            @cnt(0usize),
        )
    };
    (
        container: $container:ty,
        child: $child:ty,
        attributes: [
            $($name:ident: $attr:ty,)+
        ],
    ) => {
        $crate::configfs_attrs!(
            count:
            @container($container),
            @child($child),
            @no_child(),
            @attrs($($name $attr)+),
            @eat($($name $attr,)+),
            @assign(),
            @cnt(0usize),
        )
    };
    (count:
     @container($container:ty),
     @child($($child:ty)?),
     @no_child($($no_child:ident)?),
     @attrs($($aname:ident $aattr:ty)+),
     @eat($name:ident $attr:ty, $($rname:ident $rattr:ty,)*),
     @assign($($assign:block)*),
     @cnt($cnt:expr),
    ) => {
        $crate::configfs_attrs!(count:
                                @container($container),
                                @child($($child)?),
                                @no_child($($no_child)?),
                                @attrs($($aname $aattr)+),
                                @eat($($rname $rattr,)*),
                                @assign($($assign)* {
                                    const N: usize = $cnt;
                                    $crate::macros::paste!( [< $container:upper _ATTRS >]).add::<N, _>(& $crate::macros::paste!( [< $container:upper _ $name:upper _ATTR >]));
                                }),
                                @cnt(1usize + $cnt),
        )
    };
    (count:
     @container($container:ty),
     @child($($child:ty)?),
     @no_child($($no_child:ident)?),
     @attrs($($aname:ident $aattr:ty)+),
     @eat(),
     @assign($($assign:block)*),
     @cnt($cnt:expr),
    ) =>
    {
        $crate::configfs_attrs!(final:
                                @container($container),
                                @child($($child)?),
                                @no_child($($no_child)?),
                                @attrs($($aname $aattr)+),
                                @assign($($assign)*),
                                @cnt($cnt),
        )
    };
    (final:
     @container($container:ty),
     @child($($child:ty)?),
     @no_child($($no_child:ident)?),
     @attrs($($name:ident $attr:ty)+),
     @assign($($assign:block)+),
     @cnt($cnt:expr),
    ) =>
    {
        {
            $(
                $crate::macros::paste!{
                    static [< $container:upper _ $name:upper _ATTR >] : $crate::configfs::Attribute<$attr, $container>
                        = $crate::configfs::Attribute::new(c_str!(::core::stringify!($name)));
                }
            )+


                const N: usize = $cnt + 1usize;
            $crate::macros::paste!{
                static [< $container:upper _ATTRS >] : $crate::configfs::AttributeList<N, $container> =
                    $crate::configfs::AttributeList::new();
            }

            $($assign)+

            $(
                $crate::macros::paste!{
                    const [<$no_child:upper>]: bool = true;
                };

                $crate::macros::paste!{
                    static [< $container:upper _TPE >] : $crate::configfs::ItemType<$container>  =
                        $crate::configfs::ItemType::new::<N>(&  [<$ container:upper _ATTRS >] );
                }
            )?

            $(
                $crate::macros::paste!{
                    static [< $container:upper _TPE >] : $crate::configfs::ItemType<$container>  =
                        $crate::configfs::ItemType::new_with_child_ctor::<N, $container, $child>(&  [<$ container:upper _ATTRS >] );
                }
            )?

            &$crate::macros::paste!( [< $container:upper _TPE >] )
        }
    };

}
