
# Note: RUST_HELPERS is defined in ../Makefile

# Missing prototypes are expected in the helpers since these are exported
# for Rust only, thus there is no header nor prototypes.
obj-$(CONFIG_RUST) += $(patsubst %,%.o, $(RUST_HELPERS))
ccflags-remove-y += -Wmissing-prototypes -Wmissing-declarations
