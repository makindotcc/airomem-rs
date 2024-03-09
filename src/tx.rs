pub trait Tx<D, R = ()> {
    fn execute(self, data: &mut D) -> R;
}

/// Check tests or readme for example usage.
#[macro_export]
macro_rules! NestedTx {
    ($name:tt<$data:tt> {
        $(
            $variant:tt ($($field_name:tt : $field_type:ty),* $(,)?) -> $return_type:ty: $tx_fn:expr
        ),* $(,)?
    }) => {
        #[derive(serde::Serialize, serde::Deserialize)]
        pub enum $name {
            $(
                $variant($variant),
            )*
        }

        impl $crate::Tx<$data> for $name {
            fn execute(self, data: &mut $data) {
                match self {
                    $(
                        $name::$variant(it) => {
                            it.execute(data);
                        }
                    )*
                }
            }
        }

        $(
            $crate::Subtx! {
                #[tx($name)]
                struct $variant {
                    $(
                        $field_name: $field_type,
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
        struct $tx_struct:tt {
        }
        $tx_impl:item
    ) => {
        #[derive(Serialize, Deserialize)]
        pub struct $tx_struct;

        $crate::EnumBorrowOwned!($wrapper, $tx_struct);

        $tx_impl
    };
    (
        #[tx($wrapper:tt)]
        struct $tx_struct:tt {
            $($field_name:tt : $field_type:ty),* $(,)?
        }
        $tx_impl:item
    ) => {
        #[derive(Serialize, Deserialize)]
        pub struct $tx_struct {
            $($field_name : $field_type),*
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
