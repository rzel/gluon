use std::mem;

use base::ast;
use base::interner::InternedStr;
use vm::{VM, typecheck_expr_to, compile_expr, Error, Value, BytecodeFunction};
use api::{VMType, Generic, Getable};
use api;
use check::macros::Macro;
use check::macros::Error as MacroError;
use check::typecheck::{TcType, TcIdent};

impl VMType for ast::LExpr<TcIdent> {
    type Type = ast::LExpr<TcIdent>;

    fn make_type(vm: &VM) -> TcType {
        vm.get_type::<Self>()
            .cloned()
            .unwrap_or_else(|| panic!("Expected type to be inserted before get_type call"))
    }
}
type ExprArg = Box<ast::LExpr<TcIdent>>;
type MacroTransform = fn (ExprArg) -> ExprArg;

pub struct DefMacro;

impl <'a> Macro<VM<'a>> for DefMacro {
    fn expand(&self, vm: &VM<'a>, arguments: &mut [ast::LExpr<TcIdent>]) -> Result<ast::LExpr<TcIdent>, MacroError> {
        if let None = vm.get_type::<ast::LExpr<TcIdent>>() {
            vm.register_type::<ast::LExpr<TcIdent>>("Expr")
                .unwrap();
        }
        if arguments.len() != 2 {
            let msg = format!("Expected 'def_macro' to receive exactly 2 arguments but got {}", arguments.len());
            return Err(Error::Message(msg).into())
        }
        let name = match *arguments[0] {
            ast::Expr::Identifier(ref id) => id.name,
            _ => return Err(Error::Message("Expected 'def_macro' to receive an identifier as the first argument".into()).into())
        };
        vm.set_macro(name, RunMacro { id: name });
        let loc = arguments[0].location;
        let expr = mem::replace(&mut arguments[1], ast::located(loc, ast::Expr::Tuple(vec![])));
        let transform_type = MacroTransform::make_type(vm);
        let (expr, _typ, type_infos) = try!(typecheck_expr_to(vm, expr, Some(transform_type)));
        let function = compile_expr(vm, &type_infos, expr);
        let function = BytecodeFunction::new(&mut vm.gc.borrow_mut(), function);
        let closure = Value::Closure(vm.new_closure(function, &[]));
        let closure = Generic::<MacroTransform>::from_value(vm, closure)
            .unwrap();
        try!(vm.define_global(&name, closure));
        Ok(ast::located(arguments[0].location, ast::Expr::Tuple(vec![])))
    }
}

struct RunMacro {
    id: InternedStr
}

impl <'a> Macro<VM<'a>> for RunMacro {
    fn expand(&self, vm: &VM<'a>, arguments: &mut [ast::LExpr<TcIdent>]) -> Result<ast::LExpr<TcIdent>, MacroError> {
        println!("{} {:?}", self.id, arguments);
        if arguments.len() < 1 {
            return Err(Error::Message(format!("Expected macro '{}' to receive atleast 1 argument", self.id)).into())
        }
        //TODO Pass all arguments through
        //TODO Don't use Box and cloning for passing through these arguments
        let arg = Box::new(arguments[0].clone());
        let mut f: api::Callable<(ExprArg,), ExprArg> = match api::get_function(vm, &self.id) {
            Some(f) => f,
            None => return Err(Error::Message(format!("Expected macro function '{}' to exist", self.id)).into())
        };
        let result = try!(f.call(arg));
        Ok(*result)
    }
}

#[cfg(test)]
mod tests {
    use vm::{VM, Value, run_expr};

    #[test]
    fn id_macro() {
        let _ = ::env_logger::init();
        let text = 
r#"
let _ = def_macro id (\e -> e)
in id 4 #Int+ id 5
"#;
        let mut vm = VM::new();
        let result = run_expr(&mut vm, text).unwrap();
        assert_eq!(result, Value::Int(9));
    }
}
