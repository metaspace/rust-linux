# Rust Helpers

Non-trivial C macros cannot be used in Rust. Similarly, inlined C functions
cannot be called either. The files in this directory explicitly creates
functions ("helpers") that wrap those so that they can be called from Rust.

Even though Rust kernel modules should never use directly the bindings, some
of these helpers need to be exported because Rust generics and inlined
functions may not get their code generated in the crate where they are
defined. Other helpers, called from non-inline functions, may not be
exported, in principle. However, in general, the Rust compiler does not
guarantee codegen will be performed for a non-inline function either.
Therefore, this file exports all the helpers. In the future, this may be
revisited to reduce the number of exports after the compiler is informed
about the places codegen is required.

All symbols are exported as GPL-only to guarantee no GPL-only feature is
accidentally exposed.
