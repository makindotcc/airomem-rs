pub trait Tx<D, R = ()> {
    fn execute(self, data: &mut D) -> R;
}

/// Check tests or readme for example usage.
#[macro_export]
macro_rules! NestedTx {
    (
        $(#[$root_meta:meta])*
        $visibility:vis $name:ident<$data:tt> {
        $(
            $(#[$variant_meta:meta])*
            $variant:ident (
                $(
                    $(#[$field_meta:meta])*
                    $field_vis:vis $field_name:ident : $field_type:ty
                ),* $(,)?
            ) -> $return_type:ty = $tx_fn:expr
        ),* $(,)?
    }) => {
        #[derive(serde::Serialize, serde::Deserialize)]
        $(#[$root_meta])*
        $visibility enum $name {
            $(
                $variant($variant),
            )*
        }

        impl $crate::Tx<$data> for $name {
            fn execute(self, data: &mut $data) {
                match self {
                    $(
                        $name::$variant(it) => {
                            let _ = it.execute(data);
                        }
                    )*
                }
            }
        }

        $(
            $crate::Subtx! {
                #[tx($name)]
                $(#[$variant_meta])*
                $visibility struct $variant {
                    $(
                        $(#[$field_meta])*
                        $field_vis $field_name: $field_type,
                    )*
                }

                impl $crate::Tx<$data, $return_type> for $variant {
                    fn execute(self, data: &mut $data) -> $return_type {
                        $tx_fn(data, self)
                    }
                }
            }
        )*
    };
}

#[macro_export]
macro_rules! Subtx {
    (
        #[tx($wrapper:tt)]
        $(#[$variant_meta:meta])*
        $visibility:vis struct $tx_struct:tt {
        }
        $tx_impl:item
    ) => {
        #[derive(serde::Serialize, serde::Deserialize)]
        $(#[$variant_meta])*
        $visibility struct $tx_struct;

        $crate::EnumBorrowOwned!($wrapper, $tx_struct);

        $tx_impl
    };
    (
        #[tx($wrapper:tt)]
        $(#[$variant_meta:meta])*
        $visibility:vis struct $tx_struct:tt {
            $(
                $(#[$field_meta:meta])*
                $field_vis:vis $field_name:ident : $field_type:ty
            ),* $(,)?
        }
        $tx_impl:item
    ) => {
        #[derive(serde::Serialize, serde::Deserialize)]
        $(#[$variant_meta])*
        $visibility struct $tx_struct {
            $(
                $(#[$field_meta])*
                $field_vis $field_name : $field_type
            ),*
        }

        $crate::EnumBorrowOwned!($wrapper, $tx_struct);

        $tx_impl
    };
}

/// Implements [Into<TargetEnum>] and [From<TargetEnum>] for given struct.
/// [crate::Tx] needs to implement this, because we need to wrap nested tx temporarily into
/// its parent to serialize it, but then we need it back to execute it on data.
///
/// We can't just simply change order (serialization -> execution to execution -> serialization),
/// because serialization can throw I/O error and we don't want to get different data state than
/// its representation in file.
#[macro_export]
macro_rules! EnumBorrowOwned {
    ($wrapper:tt, $sub:tt) => {
        $crate::EnumBorrowOwned!($wrapper::$sub, $sub);
    };
    ($wrapper:tt :: $wrapper_variant:tt, $sub:tt) => {
        impl Into<$wrapper> for $sub {
            fn into(self) -> $wrapper {
                $wrapper::$wrapper_variant(self)
            }
        }

        impl From<$wrapper> for $sub {
            fn from(value: $wrapper) -> Self {
                match value {
                    $wrapper::$wrapper_variant(q) => q,
                    #[allow(unreachable_patterns)]
                    _ => unreachable!(),
                }
            }
        }
    };
}
