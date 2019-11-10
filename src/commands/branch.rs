use crate::commands::CommandContext;
use crate::repository::Repository;
use crate::revision::{Revision};

use crate::database::tree_diff;

use std::io::{Read, Write};

pub struct Branch<'a, I, O, E>
where
    I: Read,
    O: Write,
    E: Write,
{
    repo: Repository,
    ctx: CommandContext<'a, I, O, E>,
}

impl<'a, I, O, E> Branch<'a, I, O, E>
where
    I: Read,
    O: Write,
    E: Write,
{
    pub fn new(ctx: CommandContext<'a, I, O, E>) -> Branch<'a, I, O, E> {
        let working_dir = &ctx.dir;
        let root_path = working_dir.as_path();
        let repo = Repository::new(&root_path);

        let mut td = tree_diff::TreeDiff::new(root_path);
        td.compare_oids(Some("a26dcc98f714b7dbcfc7cfd9acc93ce4a79a53b8".to_string()),
                        Some("2fc5973f8c246fd66e7ab65784ffa6b2d1e6f840".to_string()), std::path::Path::new(""));
        println!("{:?}", td.changes);

        Branch { repo, ctx }
    }

    pub fn run(&mut self) -> Result<(), String> {
        self.create_branch()?;

        Ok(())
    }

    fn create_branch(&mut self) -> Result<(), String> {
        assert!(self.ctx.args.len() > 2, "no branch name provided");
        let branch_name = &self.ctx.args[2];
        let start_point = if self.ctx.args.len() < 3 {
            self.repo.refs.read_head().expect("empty HEAD")
        } else {
            match Revision::new(&mut self.repo, &self.ctx.args[3]).resolve() {
                Ok(rev) => rev,
                Err(errors) => {
                    let mut v = vec![];
                    for error in errors {
                        v.push(format!("error: {}", error.message));
                        for h in error.hint {
                            v.push(format!("hint: {}", h));
                        }
                    }

                    v.push("\n".to_string());

                    return Err(v.join("\n"));
                }
            }
        };

        self.repo.refs.create_branch(branch_name, &start_point)?;

        Ok(())
    }
}
