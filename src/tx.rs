pub trait Tx<D, R = ()> {
    fn execute(self, data: &mut D) -> R;
}

#[macro_export]
macro_rules! MergeTx {
    (
        $(#[$merged_meta:meta])*
        $visibility:vis $tx_name:ident<$data:ident> = $($subtx:ident)|*
    ) => {
        #[derive(serde::Serialize, serde::Deserialize)]
        $(#[$merged_meta])*
        $visibility enum $tx_name {
            $(
                $subtx($subtx),
            )*
        }

        impl $crate::Tx<$data> for $tx_name {
            fn execute(self, data: &mut $data) {
                match self {
                    $(
                        $tx_name::$subtx(child) => {
                            let _ = child.execute(data);
                        }
                    )*
                }
            }
        }
        $(
            $crate::EnumBorrowOwned!($tx_name, $subtx);
        )*
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
