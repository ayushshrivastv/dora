use dora_node_api::{
    self,
    arrow::{array::PrimitiveBuilder, datatypes::UInt64Type},
    dora_core::config::DataId,
    DoraNode, Event,
};

fn main() -> eyre::Result<()> {
    println!("hello");

    let output = DataId::from("random".to_owned());

    let (mut node, mut events) = DoraNode::init_from_env()?;

    for i in 0..100 {
        let event = match events.recv() {
            Some(input) => input,
            None => break,
        };

        match event {
            Event::Input {
                id,
                metadata,
                data: _,
            } => match id.as_str() {
                "tick" => {
                    let random: u64 = rand::random();
                    println!("tick {i}, sending {random:#x}");
                    let data = {
                        let mut builder: PrimitiveBuilder<UInt64Type> = PrimitiveBuilder::new();
                        builder.append_value(random);
                        builder.finish()
                    };
                    node.send_output(output.clone(), metadata.parameters, data)?;
                }
                other => eprintln!("Ignoring unexpected input `{other}`"),
            },
            Event::Stop => println!("Received manual stop"),
            other => eprintln!("Received unexpected input: {other:?}"),
        }
    }

    Ok(())
}
