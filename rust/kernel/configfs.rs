// SPDX-License-Identifier: GPL-2.0

//! Configfs interface.
//!
//! Features not covered:
//!
//! - Items. All group children are groups.
//! - Symlink support.
//! - `disconnect_notify` hook.
//! - Item `release` hook
//! - Default groups.
//!
//! See [the samples folder] for an example.
//!
//! For details on configfs, see the [`C
//! documentation`](srctree/Documentation/filesystems/configfs.rst).
//!
//! C header: [`include/linux/configfs.h`](srctree/include/linux/configfs.h)
//!
//! [the samples folder]: srctree/samples/rust/rust_configfs.rs
//!

use crate::container_of;
use crate::types::ForeignOwnable;
use crate::{prelude::*, types::Opaque};
use core::cell::UnsafeCell;
use core::marker::PhantomData;
use core::mem::offset_of;
use core::ops::Deref;
use core::ptr::addr_of;
use core::ptr::addr_of_mut;
use kernel::alloc::flags;
use kernel::str::CString;
use kernel::sync::Arc;

/// A `configfs` subsystem.
///
/// This is the top level entrypoint for a `configfs` hierarchy. Embed a field
/// of this type into a struct and implement [`HasSubsystem`] for the struct
/// with the [`kernel::impl_has_subsystem`] macro. Instantiate the subsystem with
/// [`Subsystem::register`].
///
/// A [`Subsystem`] is also a [`Group`], and implementing [`HasSubsystem`] for a
/// type will automatically implement [`HasGroup`] for the type.
#[pin_data(PinnedDrop)]
pub struct Subsystem<DATA> {
    #[pin]
    subsystem: Opaque<bindings::configfs_subsystem>,
    #[pin]
    data: DATA,
}

unsafe impl<DATA> Sync for Subsystem<DATA> {}
unsafe impl<DATA> Send for Subsystem<DATA> {}

impl<DATA> Subsystem<DATA> {
    /// Create an initializer for a [`Subsystem`].
    ///
    /// The subsystem will appear in configfs as a directory name given by
    /// `name`. The attributes available in directory are specified by
    /// `item_type`.
    pub fn new(
        name: &'static CStr,
        _module: &ThisModule,
        item_type: &'static ItemType<DATA>,
        data: impl PinInit<DATA, Error>,
    ) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            subsystem <- Opaque::ffi_init(|place: *mut bindings::configfs_subsystem| {
                unsafe {addr_of_mut!((*place).su_group.cg_item.ci_name ).write(name.as_ptr() as _) };
                unsafe {addr_of_mut!((*place).su_group.cg_item.ci_type).write(item_type.as_ptr()) };
                unsafe { bindings::config_group_init(&mut (*place).su_group) };
                unsafe { bindings::__mutex_init(&mut (*place).su_mutex, kernel::optional_name!().as_char_ptr(), kernel::static_lock_class!().as_ptr()) }
            }),
            data <- data,
        }).pin_chain(|this| {
            crate::error::to_result(unsafe {
                bindings::configfs_register_subsystem(
                    this.subsystem.get()
                )
            })
        })
    }
}

#[pinned_drop]
impl<DATA> PinnedDrop for Subsystem<DATA> {
    fn drop(self: Pin<&mut Self>) {
        unsafe { bindings::configfs_unregister_subsystem(self.subsystem.get()) };
    }
}

pub unsafe trait HasGroup<DATA> {
    unsafe fn group(this: *const Self) -> *const bindings::config_group;
    unsafe fn container_of(group: *const bindings::config_group) -> *const Self;
}

unsafe impl<DATA> HasGroup<DATA> for Subsystem<DATA> {
    unsafe fn group(this: *const Self) -> *const bindings::config_group {
        unsafe { addr_of!((*(*this).subsystem.get()).su_group) }
    }

    unsafe fn container_of(group: *const bindings::config_group) -> *const Self {
        let c_subsys_ptr = unsafe { container_of!(group, bindings::configfs_subsystem, su_group) };
        let opaque_ptr = c_subsys_ptr.cast::<Opaque<bindings::configfs_subsystem>>();
        unsafe { container_of!(opaque_ptr, Subsystem<DATA>, subsystem) }
    }
}

/// A `configfs` group.
///
/// To add a subgroup to `configfs`, embed a field of this type into a struct
/// and use it for the `CHLD` generic of [`GroupOperations`].
#[pin_data]
pub struct Group<DATA> {
    #[pin]
    group: Opaque<bindings::config_group>,
    #[pin]
    data: DATA,
}

impl<DATA> Group<DATA> {
    /// Create an initializer for a new group.
    ///
    /// When instantiated, the group will appear as a directory with the name
    /// given by `name` and it will contain attributes specified by `item_type`.
    pub fn new(
        name: CString,
        item_type: &'static ItemType<DATA>,
        data: impl PinInit<DATA, Error>,
    ) -> impl PinInit<Self, Error> {
        try_pin_init!(Self {
            group <- kernel::init::zeroed().chain(|v: &mut Opaque<bindings::config_group>| {
                let place = v.get();
                let name = name.as_bytes_with_nul().as_ptr();
                unsafe { bindings::config_group_init_type_name(place, name as _, item_type.as_ptr()) }
                Ok(())
            }),
            data <- data,
        })
    }
}

unsafe impl<DATA> HasGroup<DATA> for Group<DATA> {
    unsafe fn group(this: *const Self) -> *const bindings::config_group {
        unsafe { (*this).group.get() }
    }

    unsafe fn container_of(group: *const bindings::config_group) -> *const Self {
        let opaque_ptr = group.cast::<Opaque<bindings::config_group>>();
        unsafe { container_of!(opaque_ptr, Self, group) }
    }
}

struct GroupOperationsVTable<PAR, CHLD, CPTR, PCPTR>(PhantomData<(PAR, CHLD, CPTR, PCPTR)>)
where
    PAR: GroupOperations<PAR, CHLD, CPTR, PCPTR>,
    CPTR: InPlaceInit<Group<CHLD>, PinnedSelf = PCPTR>,
    PCPTR: ForeignOwnable<PointedTo = Group<CHLD>>;

impl<PAR, CHLD, CPTR, PCPTR> GroupOperationsVTable<PAR, CHLD, CPTR, PCPTR>
where
    PAR: GroupOperations<PAR, CHLD, CPTR, PCPTR>,
    CPTR: InPlaceInit<Group<CHLD>, PinnedSelf = PCPTR>,
    PCPTR: ForeignOwnable<PointedTo = Group<CHLD>>,
{
    unsafe extern "C" fn make_group(
        this: *mut bindings::config_group,
        name: *const kernel::ffi::c_char,
    ) -> *mut bindings::config_group {
        let is_root = unsafe { (*this).cg_subsys.is_null() }; // TODO: additional check

        let parent_data: &PAR = if !is_root {
            unsafe { &(*Group::<PAR>::container_of(this)).data }
        } else {
            unsafe { &(*Subsystem::container_of(this)).data }
        };

        let group_init = match PAR::make_group(parent_data, unsafe { CStr::from_char_ptr(name) }) {
            Ok(init) => init,
            Err(e) => return e.to_ptr(),
        };

        let child_group = CPTR::try_pin_init(group_init, flags::GFP_KERNEL);

        match child_group {
            Ok(child_group) => unsafe {
                Group::<CHLD>::group(child_group.into_foreign()).cast_mut()
            },
            Err(e) => e.to_ptr(),
        }
    }

    unsafe extern "C" fn drop_item(
        this: *mut bindings::config_group,
        item: *mut bindings::config_item,
    ) {
        let is_root = unsafe { (*this).cg_subsys.is_null() }; // TODO: additional check

        let parent_data: &PAR = if !is_root {
            unsafe { &(*Group::container_of(this)).data }
        } else {
            unsafe { &(*Subsystem::container_of(this)).data }
        };

        let c_group_ptr = unsafe { kernel::container_of!(item, bindings::config_group, cg_item) };
        let r_group_ptr = unsafe { Group::<CHLD>::container_of(c_group_ptr) };

        if PAR::HAS_DROP_ITEM {
            PAR::drop_item(parent_data, unsafe {
                PCPTR::borrow(r_group_ptr.cast_mut())
            });
        }

        unsafe { bindings::config_item_put(item) };

        let pin_child: PCPTR = unsafe { PCPTR::from_foreign(r_group_ptr.cast_mut()) };
        drop(pin_child);
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

/// Operations implemented by `configfs` groups that can create subgroups.
///
/// Implement this trait on structs that embed a [`Subsystem`] or a [`Group`].
#[vtable]
pub trait GroupOperations<PAR, CHLD, CPTR, PCPTR>
where
    CPTR: InPlaceInit<Group<CHLD>, PinnedSelf = PCPTR>,
    PCPTR: ForeignOwnable<PointedTo = Group<CHLD>>,
{
    /// The kernel will call this method in response to `mkdir(2)` in the
    /// directory representing `this`.
    ///
    /// To accept the request to create a group, implementations should
    /// instantiate a `CHLD` and return a `CPTR` to it. To prevent creation,
    /// return a suitable error.
    fn make_group(this: &PAR, name: &CStr) -> Result<impl PinInit<Group<CHLD>, Error>>;

    /// The kernel will call this method before the directory representing
    /// `_child` is removed from `configfs`.
    ///
    /// Implementations can use this method to do house keeping before
    /// `configfs` drops its reference to `CHLD`.
    fn drop_item(_this: &PAR, _child: PCPTR::Borrowed<'_>) {
        kernel::build_error!(kernel::error::VTABLE_DEFAULT_ERROR)
    }
}

/// A `configfs` attribute.
///
/// An attribute appear as a file in configfs, inside a folder that represent
/// the group that the attribute belongs to.
#[repr(transparent)]
pub struct Attribute<AO, DATA> {
    attribute: Opaque<bindings::configfs_attribute>,
    _p: PhantomData<(AO, DATA)>,
}

unsafe impl<AO, DATA> Sync for Attribute<AO, DATA> {}

unsafe impl<AO, DATA> Send for Attribute<AO, DATA> {}

impl<AO, DATA> Attribute<AO, DATA>
where
    AO: AttributeOperations<DATA>,
{
    unsafe extern "C" fn show(
        item: *mut bindings::config_item,
        page: *mut kernel::ffi::c_char,
    ) -> isize {
        let c_group: *mut bindings::config_group = item.cast(); // TODO: Use container_of
        let is_root = unsafe { (*c_group).cg_subsys.is_null() }; // TODO: additional check

        let data: &DATA = if is_root {
            unsafe { &(*Subsystem::container_of(c_group)).data }
        } else {
            unsafe { &(*Group::container_of(c_group)).data }
        };

        AO::show(data, unsafe { &mut *(page as *mut [u8; 4096]) })
    }

    unsafe extern "C" fn store(
        item: *mut bindings::config_item,
        page: *const kernel::ffi::c_char,
        size: usize,
    ) -> isize {
        let c_group: *mut bindings::config_group = item.cast(); // TODO: Use container_of
        let is_root = unsafe { (*c_group).cg_subsys.is_null() }; // TODO: additional check

        let data: &DATA = if is_root {
            unsafe { &(*Subsystem::container_of(c_group)).data }
        } else {
            unsafe { &(*Group::container_of(c_group)).data }
        };

        AO::store(data, unsafe {
            core::slice::from_raw_parts(page.cast(), size)
        });

        size as isize
    }

    /// Create a new attribute.
    ///
    /// The attribute will appear as a file with name given by `name`.
    pub const fn new(name: &'static CStr) -> Self {
        Self {
            attribute: Opaque::new(bindings::configfs_attribute {
                ca_name: name as *const _ as _,
                ca_owner: core::ptr::null_mut(),
                ca_mode: 0o660,
                show: Some(Self::show),
                store: if AO::HAS_STORE {
                    Some(Self::store)
                } else {
                    None
                },
            }),
            _p: PhantomData,
        }
    }
}

/// Operations supported by an attribute.
///
/// Implement this trait on type and pass that type as generic parameter when
/// creating an [`Attribute`]. The type carrying the implementation serve no
/// purpose other than specifying the attribute operations.
#[vtable]
pub trait AttributeOperations<DATA> {
    /// This function is called by the kernel to read the value of an attribute.
    ///
    /// Implementations should write the rendering of the attribute to `page`
    /// and return the number of bytes written.
    fn show(data: &DATA, page: &mut [u8; 4096]) -> isize;

    /// This function is called by the kernel to update the value of an attribute.
    ///
    /// Implementations should parse the value from `page` and update internal
    /// state to reflect the parsed value. Partial writes are not supported and
    /// implementations should expect the full page to arrive in one write
    /// operation.
    fn store(data: &DATA, _page: &[u8]) {
        kernel::build_error!(kernel::error::VTABLE_DEFAULT_ERROR)
    }
}

/// A list of attributes.
///
/// This type is used to construct a new [`ItemType`]. It represents a list of
/// [`Attribute`] that will appear in the directory representing a [`Group`].
/// Users should not directly instantiate this type, rather they should use the
/// [`kernel::configfs_attrs`] macro to declare a static set of attributes for a
/// group.
#[repr(transparent)]
pub struct AttributeList<const N: usize, DATA>(
    UnsafeCell<[*mut kernel::ffi::c_void; N]>,
    PhantomData<DATA>,
);
// TODO: Make attribute constructors unsafe

unsafe impl<const N: usize, DATA> Send for AttributeList<N, DATA> {}
unsafe impl<const N: usize, DATA> Sync for AttributeList<N, DATA> {}

impl<const N: usize, DATA> AttributeList<N, DATA> {
    #[doc(hidden)]
    pub const fn new() -> Self {
        Self(UnsafeCell::new([core::ptr::null_mut(); N]), PhantomData)
    }

    #[doc(hidden)]
    pub const fn add<const I: usize, O: AttributeOperations<DATA>>(
        &'static self,
        attribute: &'static Attribute<O, DATA>,
    ) {
        if I >= N - 1 {
            kernel::build_error("Invalid attribute index");
        }

        unsafe { (&mut *self.0.get())[I] = attribute as *const _ as _ };
    }
}

/// A representation of the attributes that will appear in a [`Group`].
///
/// Users should not directly instantiate objects of this type. Rather, they
/// should use the [`kernel::configfs_attrs`] macro to statically declare the
/// shape of a [`Group`].
#[pin_data]
pub struct ItemType<DATA> {
    #[pin]
    item_type: Opaque<bindings::config_item_type>,
    _p: PhantomData<DATA>,
}

unsafe impl<DATA> Sync for ItemType<DATA> {}
unsafe impl<DATA> Send for ItemType<DATA> {}

impl<DATA> ItemType<DATA> {
    #[doc(hidden)]
    pub const fn new_with_child_ctor<const N: usize, PAR, CHLD, CPTR, PCPTR>(
        attributes: &'static AttributeList<N, DATA>,
    ) -> Self
    where
        PAR: GroupOperations<PAR, CHLD, CPTR, PCPTR>,
        CPTR: InPlaceInit<Group<CHLD>, PinnedSelf = PCPTR>,
        PCPTR: ForeignOwnable<PointedTo = Group<CHLD>>,
    {
        Self {
            item_type: Opaque::new(bindings::config_item_type {
                ct_owner: core::ptr::null_mut(),
                ct_group_ops: (&GroupOperationsVTable::<PAR, CHLD, CPTR, PCPTR>::VTABLE as *const _)
                    as *mut _,
                ct_item_ops: core::ptr::null_mut(),
                ct_attrs: attributes as *const _ as _,
                ct_bin_attrs: core::ptr::null_mut(),
            }),
            _p: PhantomData,
        }
    }

    #[doc(hidden)]
    pub const fn new<const N: usize>(attributes: &'static AttributeList<N, DATA>) -> Self {
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

impl<DATA> ItemType<DATA> {
    fn as_ptr(&self) -> *const bindings::config_item_type {
        self.item_type.get()
    }
}

/// Define a list of configfs attributes statically.
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
        pointer: $pointer:ty,
        pinned: $pinned:ty,
        attributes: [
            $($name:ident: $attr:ty,)+
        ],
    ) => {
        $crate::configfs_attrs!(
            count:
            @container($container),
            @child($child, $pointer, $pinned),
            @no_child(),
            @attrs($($name $attr)+),
            @eat($($name $attr,)+),
            @assign(),
            @cnt(0usize),
        )
    };
    (count:
     @container($container:ty),
     @child($($child:ty, $pointer:ty, $pinned:ty)?),
     @no_child($($no_child:ident)?),
     @attrs($($aname:ident $aattr:ty)+),
     @eat($name:ident $attr:ty, $($rname:ident $rattr:ty,)*),
     @assign($($assign:block)*),
     @cnt($cnt:expr),
    ) => {
        $crate::configfs_attrs!(count:
                                @container($container),
                                @child($($child, $pointer, $pinned)?),
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
     @child($($child:ty, $pointer:ty, $pinned:ty)?),
     @no_child($($no_child:ident)?),
     @attrs($($aname:ident $aattr:ty)+),
     @eat(),
     @assign($($assign:block)*),
     @cnt($cnt:expr),
    ) =>
    {
        $crate::configfs_attrs!(final:
                                @container($container),
                                @child($($child, $pointer, $pinned)?),
                                @no_child($($no_child)?),
                                @attrs($($aname $aattr)+),
                                @assign($($assign)*),
                                @cnt($cnt),
        )
    };
    (final:
     @container($container:ty),
     @child($($child:ty, $pointer:ty, $pinned:ty)?),
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
                        $crate::configfs::ItemType::new_with_child_ctor::<N, $container, $child, $pointer, $pinned>(&  [<$ container:upper _ATTRS >] );
                }
            )?

            &$crate::macros::paste!( [< $container:upper _TPE >] )
        }
    };

}
