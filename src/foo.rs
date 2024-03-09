// // struct Store<D, S> {
// //     data: D,
// //     serializer: S,
// // }

// // impl<D, S> Store<D, S> {
// //     fn open<T>(serializer: S)
// //     where
// //         S: Serializer<T>,
// //     {
// //         let bytes = vec![0, 1, 2, 3];
// //         let transactions = serializer.deserialize(&bytes[..]).unwrap().unwrap();
// //     }

// //     fn execute<T>(&mut self, tx: T) -> T::Result
// //     where
// //         T: Tx,
// //         S: Serializer<T>,
// //     {
// //         let serialized = self.serializer.serialize(&tx).unwrap();
// //         tx.execute(&mut self.data)
// //     }
// // }

// // trait Tx {
// //     type Data;
// //     type Result;

// //     fn execute(&self, data: &mut Self::Data) -> Self::Result;
// // }

// // trait Serializer<T> {
// //     type Error: std::error::Error + Send + Sync + 'static;

// //     fn serialize(&self, value: &T) -> Result<Vec<u8>, Self::Error>;

// //     fn deserialize<R>(&self, reader: R) -> Result<Option<T>, Self::Error>
// //     where
// //         R: std::io::Read;
// // }

// // struct HujSerializer;

// // impl<T> Serializer<T> for HujSerializer {
// //     type Error = ();

// //     fn serialize(&self, value: &T) -> Result<Vec<u8>, Self::Error> {
// //         todo!()
// //     }

// //     fn deserialize<R>(&self, reader: R) -> Result<Option<T>, Self::Error>
// //     where
// //         R: std::io::Read,
// //     {
// //         todo!()
// //     }
// // }

// // struct IncreaseTx;

// // impl Tx for IncreaseTx {
// //     type Data = usize;
// //     type Result = ();

// //     fn execute(&self, data: &mut usize) -> Self::Result {
// //         todo!()
// //     }
// // }

// // fn test() {
// //     let mut store = Store {
// //         data: 123,
// //         serializer: HujSerializer,
// //     };
// //     store.execute(IncreaseTx);
// // }

// // use core::fmt;

// // struct Foo {
// //     bar: Bar,
// // }

// // #[derive(Debug)]
// // struct Bar {
// //     essa: String,
// // }

// // impl Into<Foo> for Bar {
// //     fn into(self) -> Foo {
// //         Foo { bar: self }
// //     }
// // }

// // fn essa<'a, C>(xd: C)
// // where
// //     C: Into<&'a Foo> + fmt::Debug,
// // {
// //     {
// //         let foo: &'a Foo = xd.into();
// //     }
// //     println!("xd: {xd:?}");
// // }

// // #[cfg(test)]
// // mod tests {
// //     use super::{essa, Bar};

// //     #[test]
// //     fn test() {
// //         essa(Bar {
// //             essa: "dasdas".to_string(),
// //         });
// //     }
// // }

// #[derive(Debug)]
// enum Foo {
//     A,
//     B(Bar),
// }

// #[derive(Debug)]
// struct Bar {
//     name: String,
// }

// impl Bar {
//     fn hello(&self) -> impl FnOnce() {
//         println!("hello {}", self.name);

//         || {
            
//         }
//     }
// }

// fn test(bar: Bar) {
//     let serialized = serialize(&Foo::B(bar));
//     bar.hello();
// }

// fn serialize(foo: &Foo) -> Vec<u8> {
//     println!("serializing: {foo:?}");
//     // serialization...
//     vec![0, 1, 2] // mocked serialization
// }
