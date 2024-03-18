pub trait Tx<D, R = ()> {
    fn execute(self, data: &mut D) -> R;
}

/// Merges multiple [Tx] into one enum of [Tx] and
/// implements [Into] and [From] traits required by [crate::Store] for
/// merged tx (`$subtx`) variants (see [EnumBorrowOwned!]).
///
/// For more details, please use cargo-expand or check macro's code.
///
/// Store accepts only one [Tx] type, so to handle more than one [Tx] implementation,
/// we need to wrap it like in following example:
/// # Example
/// ```rust
/// struct Counter {
///     count: usize,
/// }
///
/// airomem::MergeTx!(pub CounterTx<Counter> = IncreaseTx | DecreaseTx);
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// struct IncreaseTx;
///
/// impl airomem::Tx<Counter> for IncreaseTx {
///     fn execute(self, data: &mut Counter) {
///         data.count += 1;
///     }
/// }
///
/// #[derive(serde::Serialize, serde::Deserialize)]
/// struct DecreaseTx;
///
/// impl airomem::Tx<Counter> for DecreaseTx {
///     fn execute(self, data: &mut Counter) {
///         data.count -= 1;
///     }
/// }
/// ```
/// See tests for more examples.
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
