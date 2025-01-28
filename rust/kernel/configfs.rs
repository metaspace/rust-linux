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
//! See [the samples folder] for an example.
//!
//! For details on configfs, see the [`C
//! documentation`](srctree/Documentation/filesystems/configfs.rst).
//!
//! C header: [`include/linux/configfs.h`](srctree/include/linux/configfs.h)
//!
//! [the samples folder]: srctree/samples/rust/rust_configfs.rs
//!

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
    /// Create an initializer for a [`Subsystem`].
    ///
    /// The subsystem will appear in configfs as a directory name given by
    /// `name`. The attributes available in directory are specified by
    /// `item_type`.
    pub fn new(
        name: &'static CStr,
        _module: &ThisModule,
        item_type: &'static ItemType<C>,
    ) -> impl PinInit<Self> {
        pin_init!(Self {
            subsystem <- Opaque::ffi_init(|place: *mut bindings::configfs_subsystem| {
                unsafe {addr_of_mut!((*place).su_group.cg_item.ci_name ).write(name.as_ptr() as _) };
                unsafe {addr_of_mut!((*place).su_group.cg_item.ci_type).write(item_type.as_ptr()) };
                unsafe { bindings::config_group_init(&mut (*place).su_group) };
                unsafe { bindings::__mutex_init(&mut (*place).su_mutex, kernel::optional_name!().as_char_ptr(), kernel::static_lock_class!().as_ptr()) }
            }),
            _p: PhantomData,
        })
    }

    /// Get a pointer to the group embedded within this subsystem.
    pub unsafe fn group_ptr(this: *const Self) -> *const Group<C> {
        let subsystem = this.cast::<bindings::configfs_subsystem>();
        unsafe { addr_of!((*subsystem).su_group) }.cast()
    }
}

impl<C> Subsystem<C>
where
    C: HasSubsystem,
{
    /// Register a subsystem with `configfs`.
    ///
    /// This function will instantiate a [`C: HasSubsystem`] and register the subsystem within it.
    ///
    /// [`C: HasSubsystem`]: `HasSubsystem`
    pub fn register(init: impl PinInit<C, Error>) -> Result<Registration<C>> {
        let this = Registration {
            inner: Arc::pin_init(init, flags::GFP_KERNEL)?,
        };

        crate::error::to_result(unsafe {
            bindings::configfs_register_subsystem(
                C::subsystem_ptr(this.inner.deref() as *const C)
                    .cast_mut()
                    .cast(),
            )
        })?;

        Ok(this)
    }
}

/// A registration of a `configfs` [`Subsystem`].
///
/// When the registration is droped, the registered subsystem is removed from
/// `configfs`.
pub struct Registration<C>
where
    C: HasSubsystem,
{
    inner: Arc<C>,
}

impl<C> Drop for Registration<C>
where
    C: HasSubsystem,
{
    fn drop(&mut self) {
        unsafe {
            bindings::configfs_unregister_subsystem(
                C::subsystem_ptr(self.inner.deref() as *const C)
                    .cast_mut()
                    .cast(),
            )
        };
    }
}

/// A `configfs` group.
///
/// To add a subgroup to `configfs`, embed a field of this type into a struct
/// and use it for the `CHLD` generic of [`GroupOperations`].
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
    /// Create an initializer for a new group.
    ///
    /// When instantiated, the group will appear as a directory with the name
    /// given by `name` and it will contain attributes specified by `item_type`.
    pub fn new(name: CString, item_type: &'static ItemType<C>) -> impl PinInit<Self> {
        pin_init!(Self {
            group <- kernel::init::zeroed().chain(|v: &mut Opaque<bindings::config_group>| {
                let place = v.get();
                let name = name.as_bytes_with_nul().as_ptr();
                unsafe { bindings::config_group_init_type_name(place, name as _, item_type.as_ptr()) }
                Ok(())
            }),
            _p: PhantomData,
        })
    }
}

struct GroupOperationsVTable<PAR, PPTR, CHLD, CPTR, PCPTR>(
    PhantomData<(PAR, PPTR, CHLD, CPTR, PCPTR)>,
)
where
    PAR: GroupOperations<PAR, PPTR, CHLD, CPTR, PCPTR> + HasGroup,
    PPTR: ForeignOwnable<PointedTo = PAR>,
    CHLD: HasGroup,
    CPTR: InPlaceInit<CHLD, PinnedSelf = PCPTR>,
    PCPTR: ForeignOwnable<PointedTo = CHLD>;

impl<PAR, PPTR, CHLD, CPTR, PCPTR> GroupOperationsVTable<PAR, PPTR, CHLD, CPTR, PCPTR>
where
    PAR: GroupOperations<PAR, PPTR, CHLD, CPTR, PCPTR> + HasGroup + 'static,
    PPTR: ForeignOwnable<PointedTo = PAR>,
    CHLD: HasGroup + 'static,
    CPTR: InPlaceInit<CHLD, PinnedSelf = PCPTR>,
    PCPTR: ForeignOwnable<PointedTo = CHLD>,
{
    unsafe extern "C" fn make_group(
        parent_group: *mut bindings::config_group,
        name: *const kernel::ffi::c_char,
    ) -> *mut bindings::config_group {
        let r_group_ptr: *mut Group<PAR> = parent_group.cast();
        let container_ptr = unsafe { PAR::container_ptr(r_group_ptr) };
        let container_ref = unsafe { PPTR::borrow(container_ptr) };
        let child_init = match PAR::make_group(container_ref, unsafe { CStr::from_char_ptr(name) })
        {
            Ok(child) => child,
            Err(e) => return e.to_ptr(),
        };

        let child = CPTR::try_pin_init(child_init, flags::GFP_KERNEL);

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
        let parent = unsafe { PPTR::borrow(container_ptr) };

        let c_group_ptr = unsafe { kernel::container_of!(item, bindings::config_group, cg_item) };
        let r_group_ptr: *mut Group<CHLD> = c_group_ptr.cast::<Group<CHLD>>().cast_mut();
        let container_ptr = unsafe { CHLD::container_ptr(r_group_ptr) };

        if PAR::HAS_DROP_ITEM {
            PAR::drop_item(parent, unsafe { PCPTR::borrow(container_ptr) });
        }

        unsafe { bindings::config_item_put(item) };
        let child: PCPTR = unsafe { PCPTR::from_foreign(container_ptr) };
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

/// Operations implemented by `configfs` groups that can create subgroups.
///
/// Implement this trait on structs that embed a [`Subsystem`] or a [`Group`].
#[vtable]
pub trait GroupOperations<PAR, PPTR, CHLD, CPTR, PCPTR>
where
    PAR: HasGroup,
    PPTR: ForeignOwnable<PointedTo = PAR>,
    CHLD: HasGroup,
    CPTR: InPlaceInit<CHLD, PinnedSelf = PCPTR>,
    PCPTR: ForeignOwnable<PointedTo = CHLD>,
{
    /// The kernel will call this method in response to `mkdir(2)` in the
    /// directory representing `this`.
    ///
    /// To accept the request to create a group, implementations should
    /// instantiate a `CHLD` and return a `CPTR` to it. To prevent creation,
    /// return a suitable error.
    fn make_group(this: PPTR::Borrowed<'_>, name: &CStr) -> Result<impl PinInit<CHLD, Error>>;

    /// The kernel will call this method before the directory representing
    /// `_child` is removed from `configfs`.
    ///
    /// Implementations can use this method to do house keeping before
    /// `configfs` drops its reference to `CHLD`.
    fn drop_item(_this: PPTR::Borrowed<'_>, _child: PCPTR::Borrowed<'_>) {
        kernel::build_error!(kernel::error::VTABLE_DEFAULT_ERROR)
    }
}

/// A `configfs` attribute.
///
/// An attribute appear as a file in configfs, inside a folder that represent
/// the group that the attribute belongs to.
#[repr(transparent)]
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
pub trait AttributeOperations<AO>
where
    AO: HasGroup,
{
    /// This function is called by the kernel to read the value of an attribute.
    ///
    /// Implementations should write the rendering of the attribute to `page`
    /// and return the number of bytes written.
    fn show(container: &AO, page: &mut [u8; 4096]) -> isize;

    /// This function is called by the kernel to update the value of an attribute.
    ///
    /// Implementations should parse the value from `page` and update internal
    /// state to reflect the parsed value. Partial writes are not supported and
    /// implementations should expect the full page to arrive in one write
    /// operation.
    fn store(_container: &AO, _page: &[u8]) {
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
pub struct AttributeList<const N: usize, C>(
    UnsafeCell<[*mut kernel::ffi::c_void; N]>,
    PhantomData<C>,
)
where
    C: HasGroup;
unsafe impl<const N: usize, C: HasGroup> Send for AttributeList<N, C> {}
unsafe impl<const N: usize, C: HasGroup> Sync for AttributeList<N, C> {}

impl<const N: usize, C: HasGroup> AttributeList<N, C> {
    #[doc(hidden)]
    pub const fn new() -> Self {
        Self(UnsafeCell::new([core::ptr::null_mut(); N]), PhantomData)
    }

    #[doc(hidden)]
    pub const fn add<const I: usize, O: AttributeOperations<C>>(
        &'static self,
        attribute: &'static Attribute<O, C>,
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
pub struct ItemType<C> {
    #[pin]
    item_type: Opaque<bindings::config_item_type>,
    _p: PhantomData<C>,
}

unsafe impl<C> Sync for ItemType<C> {}
unsafe impl<C> Send for ItemType<C> {}

impl<C: HasGroup> ItemType<C> {
    #[doc(hidden)]
    pub const fn new_with_child_ctor<const N: usize, PAR, PPTR, CHLD, CPTR, PCPTR>(
        attributes: &'static AttributeList<N, C>,
    ) -> Self
    where
        PAR: GroupOperations<PAR, PPTR, CHLD, CPTR, PCPTR> + HasGroup + 'static,
        PPTR: ForeignOwnable<PointedTo = PAR>,
        CHLD: HasGroup + 'static,
        CPTR: InPlaceInit<CHLD, PinnedSelf = PCPTR>,
        PCPTR: ForeignOwnable<PointedTo = CHLD>,
    {
        Self {
            item_type: Opaque::new(bindings::config_item_type {
                ct_owner: core::ptr::null_mut(),
                ct_group_ops: (&GroupOperationsVTable::<PAR, PPTR, CHLD, CPTR, PCPTR>::VTABLE
                    as *const _) as *mut _,
                ct_item_ops: core::ptr::null_mut(),
                ct_attrs: attributes as *const _ as _,
                ct_bin_attrs: core::ptr::null_mut(),
            }),
            _p: PhantomData,
        }
    }

    #[doc(hidden)]
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

/// Implement this trait for structs that embed a field of type [`Group`].
///
/// # Safety
///
/// Implementers of this trait must have a field of type [`Group`] at offset
/// `OFFSET`. If any member methods are implemented they must be implemented
/// according to the documentation on the methods in this trait declaration.
pub unsafe trait HasGroup {
    /// The implementer of the trait must have a field of type [`Group`] at this
    /// offset.
    const OFFSET: usize;

    /// Get a pointer to the field of type [`Group`] from a pointer to `Self`.
    unsafe fn group_ptr(this: *const Self) -> *const Group<Self>
    where
        Self: Sized,
    {
        unsafe { this.cast::<u8>().add(Self::OFFSET).cast::<Group<Self>>() }
    }

    /// Get a pointer to `Self` from a pointer to the field of type [`Group`].
    unsafe fn container_ptr(group: *mut Group<Self>) -> *mut Self
    where
        Self: Sized,
    {
        unsafe { group.cast::<u8>().sub(Self::OFFSET).cast::<Self>() }
    }
}

/// Implement this trait for structs that embed a field of type [`Subsystem`].
///
/// # Safety
///
/// Implementers of this trait must have a field of type [`Subsystem`] at offset
/// `OFFSET`. If any member methods are implemented they must be implemented
/// according to the documentation on the methods in this trait declaration.
pub unsafe trait HasSubsystem {
    /// The implementer of the trait must have a field of type [`Subsystem`] at
    /// this offset.
    const OFFSET: usize;

    /// Get a pointer to the field of type [`Subsystem`] from a pointer to `Self`.
    unsafe fn subsystem_ptr(this: *const Self) -> *const Subsystem<Self>
    where
        Self: Sized,
    {
        unsafe {
            this.cast::<u8>()
                .add(Self::OFFSET)
                .cast::<Subsystem<Self>>()
        }
    }

    /// Get a pointer to `Self` from a pointer to the field of type [`Subsystem`].
    unsafe fn container_ptr(subsystem: *mut Subsystem<Self>) -> *mut Self
    where
        Self: Sized,
    {
        unsafe { subsystem.cast::<u8>().sub(Self::OFFSET).cast::<Self>() }
    }
}

unsafe impl<T> HasGroup for T
where
    T: HasSubsystem,
{
    const OFFSET: usize =
        <T as HasSubsystem>::OFFSET + offset_of!(bindings::configfs_subsystem, su_group);
}

/// Use this macro to implement the [`HasGroup<T>`] trait for types that embed a
/// [`Group`].
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
            unsafe fn group_ptr(this: *const Self) ->
                *const $crate::configfs::Group<Self>
            {
                // SAFETY: The caller promises that the pointer is not dangling.
                unsafe {
                    ::core::ptr::addr_of!((*this).$field)
                }
            }
        }
    }
}

/// Use to implement the [`HasSubsystem<T>`] trait for types that embed a
/// [`Subsystem`].
#[macro_export]
macro_rules! impl_has_subsystem {
    (
        impl$({$($generics:tt)*})?
            HasSubsystem
            for $self:ty
        { self.$field:ident }
        $($rest:tt)*
    ) => {
        // SAFETY: This implementation of `group_ptr` only compiles if the
        // field has the right type.
        unsafe impl$(<$($generics)*>)? $crate::configfs::HasSubsystem for $self {
            const OFFSET: usize = ::core::mem::offset_of!(Self, $field) as usize;

            #[inline]
            unsafe fn subsystem_ptr(this: *const Self) ->
                *const $crate::configfs::Subsystem<Self>
            {
                // SAFETY: The caller promises that the pointer is not dangling.
                unsafe {
                    ::core::ptr::addr_of!((*this).$field)
                }
            }
        }
    }
}

/// Define a list of configfs attributes statically.
///
/// # Example
///
/// ```ignore
/// use kernel::configfs;
/// use kernel::configfs_attrs;
/// use kernel::prelude::*;
/// use kernel::sync::Arc;
/// use kernel::sync::ArcBorrow;
/// use kernel::c_str;
/// use kernel::types::ForeignOwnable;
///
/// #[pin_data]
/// struct Configuration {
///     #[pin]
///     subsystem: configfs::Subsystem<Self>,
/// }
///
/// kernel::impl_has_subsystem! {
///     impl HasSubsystem for Configuration { self.subsystem }
/// }
///
/// #[vtable]
/// impl configfs::GroupOperations<Configuration, Arc<Configuration>, Child, Arc<Child>, Arc<Child>> for Configuration {
///     fn make_group(_this: <Arc<Configuration> as ForeignOwnable>::Borrowed<'_>, name: &CStr) -> Result<impl PinInit<Child, Error>> {
///         todo!()
///     }
/// }
///
/// #[pin_data]
/// struct Child {
///     #[pin]
///     group: configfs::Group<Self>,
/// }
///
/// kernel::impl_has_group! {
///     impl HasGroup for Child { self.group }
/// }
///
/// enum FooOps {}
///
/// #[vtable]
/// impl configfs::AttributeOperations<Configuration> for FooOps {
///     fn show(container: &Configuration, page: &mut [u8; 4096]) -> isize {
///         pr_info!("Show foo\n");
///         todo!()
///     }
/// }
///
/// let item_type  = configfs_attrs! {
///     container: Configuration,
///     child: Child,
///     pointer: Arc<Child>,
///     pinned: Arc<Child>,
///     attributes: [
///         foo: FooOps,
///     ],
/// };
/// ```
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
                    // TODO: Parent not always Arc<$container>
                    static [< $container:upper _TPE >] : $crate::configfs::ItemType<$container>  =
                        $crate::configfs::ItemType::new_with_child_ctor::<N, $container, Arc<$container>, $child, $pointer, $pinned>(&  [<$ container:upper _ATTRS >] );
                }
            )?

            &$crate::macros::paste!( [< $container:upper _TPE >] )
        }
    };

}
