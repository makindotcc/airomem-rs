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

        $crate::ImplFromInto!($wrapper, $tx_struct);

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

        $crate::ImplFromInto!($wrapper, $tx_struct);

        $tx_impl
    };
}

#[macro_export]
macro_rules! ImplFromInto {
    ($wrapper:tt, $sub:tt) => {
        $crate::ImplFromInto!($wrapper::$sub, $sub);
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
