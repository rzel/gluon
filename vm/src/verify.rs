use std::mem;
use std::rc::Rc;

use base::types::{TypeEnv, TcType, Type, arg_iter};
use base::error::Errors;
use types::{VMIndex, Instruction};

use check::equivalent;

#[derive(Clone, Debug)]
pub enum AbstractType {
    TcType(TcType),
    Variant(VMIndex, Rc<Vec<AbstractType>>),
}

impl From<TcType> for AbstractType {
    fn from(t: TcType) -> AbstractType {
        AbstractType::TcType(t)
    }
}

pub enum Error {
    UndefinedGlobal(VMIndex),
    TypeMismatch(AbstractType, AbstractType),
    NotEnoughArguments,
    EmptyStack,
    FieldIsOutOfRange(AbstractType, VMIndex),
}

pub struct Verifier<'a> {
    type_env: &'a (TypeEnv + 'a),
    globals: &'a (Fn(usize) -> Option<TcType> + 'a),
    stack: Vec<AbstractType>,
    errors: Errors<Error>,
}


impl<'a> Verifier<'a> {
    pub fn new(type_env: &'a (TypeEnv + 'a),
               globals: &'a Fn(usize) -> Option<TcType>)
               -> Verifier<'a> {
        Verifier {
            type_env: type_env,
            globals: globals,
            stack: Vec::new(),
            errors: Errors::new(),
        }
    }

    pub fn verify(&mut self,
                  bytecode_type: &TcType,
                  instructions: &[Instruction])
                  -> Result<(), Errors<Error>> {
        use types::Instruction::*;

        self.stack.clear();
        for arg in arg_iter(bytecode_type) {
            self.stack.push(arg.clone().into());
        }
        for &inst in instructions {
            match inst {
                Push(i) => {
                    let t = self.stack[i as usize].clone();
                    self.stack.push(t);
                }
                PushInt(_) => self.stack.push(Type::int().into()),
                PushFloat(_) => self.stack.push(Type::float().into()),
                PushString(_) => self.stack.push(Type::string().into()),
                PushGlobal(offset) => {
                    match (self.globals)(offset as usize) {
                        Some(typ) => self.stack.push(typ.into()),
                        None => self.errors.error(Error::UndefinedGlobal(offset)),
                    }
                }
                Call(args) | TailCall(args) => {
                    if self.stack.len() <= args as usize + 1 {
                        self.errors.error(Error::NotEnoughArguments);
                        return Err(mem::replace(&mut self.errors, Errors::new()));
                    }
                    let return_type = {
                        let call = self.stack[self.stack.len() - args as usize - 1..].split_first();
                        let (function_type, arg_types) = match call {
                            Some((&AbstractType::TcType(ref f), arg_types)) => (f, arg_types),
                            _ => panic!(),
                        };
                        let mut expected_iter = arg_iter(function_type);
                        for (expected, actual) in expected_iter.by_ref().zip(arg_types) {
                            let expected = AbstractType::TcType(expected.clone());
                            if !self.equivalent(&expected, actual) {
                                self.errors
                                    .error(Error::TypeMismatch(expected, actual.clone()));
                            }
                        }
                        AbstractType::TcType(expected_iter.typ.clone())
                    };
                    for _ in 0..(args + 1) {
                        self.stack.pop();
                    }
                    self.stack.push(return_type);
                }
                Construct(tag, args) => {
                    let i = self.stack.len() - args as usize;
                    let arg_types = Rc::new(self.stack.split_off(i));
                    self.stack.push(AbstractType::Variant(tag, arg_types));
                }
                GetField(offset) => {
                    let top = match self.stack.pop() {
                        Some(top) => top,
                        None => {
                            self.errors.error(Error::EmptyStack);
                            continue;
                        }
                    };
                    let maybe_type = match top {
                        AbstractType::TcType(ref typ) => {
                            match **typ {
                                Type::Record { ref fields, .. } => {
                                    fields.get(offset as usize).map(|field| AbstractType::TcType(field.typ.clone()))
                                }
                                _ => None
                            }
                        }
                        AbstractType::Variant(_tag, ref args) => {
                            args.get(offset as usize).cloned()
                        }
                    };
                    match maybe_type {
                        Some(typ) => {
                            self.stack.push(typ);
                        }
                        None => {
                            self.errors.error(Error::FieldIsOutOfRange(top, offset));
                        }
                    }
                }
                _ => return Err(Errors::new()),
            }
        }
        if self.errors.has_errors() {
            Err(mem::replace(&mut self.errors, Errors::new()))
        } else {
            Ok(())
        }
    }

    fn equivalent(&self, expected: &AbstractType, actual: &AbstractType) -> bool {
        match (expected, actual) {
            (&AbstractType::TcType(ref expected),
             &AbstractType::TcType(ref actual)) => equivalent(self.type_env, expected, actual),
            (&AbstractType::TcType(ref expected),
             &AbstractType::Variant(actual_tag, ref actual_args)) => {
                match **expected {
                    Type::Variants(ref variants) => {
                        variants.get(actual_tag as usize)
                                .map(|t| {
                                    arg_iter(&t.1)
                                        .zip(&**actual_args)
                                        .all(|(l, r)| {
                                            self.equivalent(&AbstractType::TcType(l.clone()), r)
                                        })
                                })
                                .unwrap_or(false)
                    }
                    _ => false,
                }
            }
            (&AbstractType::Variant(expected_tag, ref expected_args),
             &AbstractType::TcType(ref actual)) => {
                match **actual {
                    Type::Variants(ref variants) => {
                        variants.get(expected_tag as usize)
                                .map(|t| {
                                    expected_args.iter()
                                                 .zip(arg_iter(&t.1))
                                                 .all(|(l, r)| self.equivalent(l, &AbstractType::TcType(r.clone())))
                                })
                                .unwrap_or(false)
                    }
                    _ => false,
                }
            }
            (&AbstractType::Variant(expected_tag, ref expected_args),
             &AbstractType::Variant(actual_tag, ref actual_args)) => {
                expected_tag == actual_tag &&
                expected_args.iter()
                             .zip(&**actual_args)
                             .all(|(l, r)| self.equivalent(l, r))
            }
        }
    }
}
