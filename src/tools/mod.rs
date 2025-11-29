pub mod registry;

// pub mod registry {
//     pub struct ToolRegistry {
//         tools: Vec<Box<dyn Tool>>,
//     }

//     impl ToolRegistry {
//         pub fn new() -> Self {
//             ToolRegistry { tools: Vec::new() }
//         }

//         pub fn register(&mut self, tool: Box<dyn Tool>) {
//             self.tools.push(tool);
//         }

//         pub fn get_tools(&self) -> &Vec<Box<dyn Tool>> {
//             &self.tools
//         }
//     }
// }
