//! Stanley is a Rust compiler plugin to verify functions annotated with
//! preconditions and postconditions.

#![feature(plugin_registrar, rustc_private)]

extern crate z3;
extern crate syntax;

#[macro_use]
extern crate rustc;
extern crate rustc_plugin;
extern crate rustc_trans;
extern crate rustc_data_structures;

mod ast;
mod condition_parser;

use z3::*;
use rustc_plugin::Registry;
use rustc::mir::transform::{Pass, MirPass, MirSource};
use rustc::mir::*;
use rustc::ty::{TyCtxt, Ty};
use syntax::feature_gate::AttributeType;
use syntax::codemap::Spanned;
use syntax::ast::{MetaItemKind, NestedMetaItemKind, Attribute_};
use ast::{Expression, Types};

struct StanleyMir;

pub struct MirData<'tcx> {
    block_data: Vec<&'tcx BasicBlockData<'tcx>>,
    arg_data: Vec<&'tcx LocalDecl<'tcx>>,
    var_data: Vec<&'tcx LocalDecl<'tcx>>,
    temp_data: Vec<&'tcx LocalDecl<'tcx>>,
    func_return_type: Ty<'tcx>,
}

impl <'tcx> Pass for StanleyMir {}

impl <'tcx> MirPass<'tcx> for StanleyMir {
    fn run_pass<'a>(&mut self, tcx: TyCtxt<'a, 'tcx, 'tcx>, src: MirSource, mir: &mut Mir<'tcx>) {
        let item_id = src.item_id();
        let def_id = tcx.map.local_def_id(item_id);
        let name = tcx.item_path_str(def_id);
        let attrs = tcx.map.attrs(item_id);

        let (pre_string, post_string) = parse_attributes(attrs);

        if pre_string == "" || post_string == "" {
            return;
        }

        let mut pre_string_expression = parse_condition(pre_string);
        let mut post_string_expression = parse_condition(post_string);

        let mut data = MirData {
            block_data: Vec::new(),
            arg_data: Vec::new(),
            var_data: Vec::new(),
            temp_data: Vec::new(),
            func_return_type: mir.return_ty
        };

        for block in mir.basic_blocks() {
            data.block_data.push(block);
        }

        for arg_data in mir.args_iter() {
            data.arg_data.push(&mir.local_decls[arg_data]);
        }

        for var_data in mir.vars_iter() {
            data.var_data.push(&mir.local_decls[var_data]);
        }

        for temp_data in mir.temps_iter() {
            data.temp_data.push(&mir.local_decls[temp_data]);
        }

        pre_string_expression = walk_and_replace(pre_string_expression, &data);
        post_string_expression = walk_and_replace(post_string_expression, &data);

        ast::ty_check(&pre_string_expression).unwrap();
        ast::ty_check(&post_string_expression).unwrap();

        /*let weakest_precondition = gen(0, &mut data, &post_expr, debug);

        // Create the verification condition, P -> WP
        let verification_condition: Expression = Expression::BinaryExpression( BinaryExpressionData{
            op: BinaryOperator::Implication,
            left: Box::new(pre_expr.as_ref().unwrap().clone()),
            right: Box::new(weakest_precondition.as_ref().unwrap().clone())
        } );

        // FIXME: Debug should not be a const; it must be user-facing
        if debug {
            println!("vc: {}\n", verification_condition);
        }
        // Check that the verification condition is correctly typed
        match expression::ty_check(&verification_condition) {
            Ok(_) => {},
            Err(e) => rp_error!("{}", e),
        }*/

        gen_smtlib(&post_string_expression, name);
        //gen_smtlib(&verification_condition, name);

        println!("\n");
    }
}

fn gen_smtlib(expression: &Expression, name: String) {
    let cfg = Config::new();
    let ctx = Context::new(&cfg);

    let x = ctx.named_int_const("x");
    let y = ctx.named_int_const("y");
    let zero = ctx.from_i64(0);
    let two = ctx.from_i64(2);
    let seven = ctx.from_i64(7);

    let solver = Solver::new(&ctx);
    solver.assert(&x.gt(&y));
    solver.assert(&y.gt(&zero));
    solver.assert(&y.rem(&seven)._eq(&two));
    solver.assert(&x.add(&[&two]).gt(&seven));
    assert!(solver.check());

    let model = solver.get_model();
    let xv = model.eval(&x).unwrap().as_i64().unwrap();
    let yv = model.eval(&y).unwrap().as_i64().unwrap();
    println!("x: {}, y: {}", xv, yv);
}

fn get_argument_type(name: String, data: &MirData) -> Types {
    for arg in data.arg_data.iter() {
        let a = arg.name.unwrap().as_str();
        let arg_name = String::from_utf8_lossy(a.as_bytes());

        if name == arg_name {
            return ast::type_to_enum(arg.ty)
        }
    }
    Types::Unknown
}

fn walk_and_replace(expression: Expression, data: &MirData) -> Expression {
    match expression {
        Expression::VariableMapping(a, b) => {
            let aa = a.clone();
            let mut bb = b.clone();

            if bb == Types::Unknown {
                if aa == "ret" {
                    bb = ast::type_to_enum(data.func_return_type);
                } else {
                    bb = get_argument_type(a, data);
                }
            }

            Expression::VariableMapping(aa, bb)
        },
        Expression::BinaryExpression(a, b, c) => {
            let aa = walk_and_replace(*a.clone(), data);
            let ba = b.clone();
            let ca = walk_and_replace(*c.clone(), data);
            Expression::BinaryExpression(Box::new(aa), ba, Box::new(ca))
        },
        Expression::UnaryExpression(a, b) => {
            let aa = a.clone();
            let ba = walk_and_replace(*b.clone(), data);
            Expression::UnaryExpression(aa, Box::new(ba))
        },
        _ => expression.clone()
    }
}

fn parse_condition(condition: String) -> Expression {
    match condition_parser::parse_Condition(&*condition) {
        Ok(e) => e,
        Err(e) => panic!("Error parsing condition \"{}\" with error \"{:?}\"", condition, e)
    }
}

fn parse_attributes(attrs: &[Spanned<Attribute_>]) -> (String, String) {
    let mut pre_string = "".to_string();
    let mut post_string = "".to_string();

    for attr in attrs {
        if let MetaItemKind::List(_, ref items) = attr.node.value.node {
            for item in items {
                if let NestedMetaItemKind::MetaItem(ref i_string) = item.node {
                    if let MetaItemKind::NameValue(ref attr_param_name, ref literal) = i_string.node {
                        if let syntax::ast::LitKind::Str(ref attr_param_value, _) = literal.node {

                            match attr_param_name.to_string().as_ref() {
                                "pre" => pre_string = attr_param_value.to_string(),
                                "post" => post_string = attr_param_value.to_string(),
                                _ => panic!("I only accept `pre` and `post`. You gave me \"{}\"", attr_param_name)
                            }
                        }
                    }
                }
            }
        }
    }

    (pre_string, post_string)
}

#[plugin_registrar]
pub fn plugin_registrar(reg: &mut Registry) {
	let stanleymir = StanleyMir {};
    reg.register_attribute("condition".to_string(), AttributeType::Whitelisted);
    reg.register_mir_pass(Box::new(stanleymir));
}