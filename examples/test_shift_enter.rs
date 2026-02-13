use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::io;

fn main() -> io::Result<()> {
    println!("Testing Shift+Enter detection...");
    println!("Press Shift+Enter (should see SHIFT modifier)");
    println!("Press plain Enter (should see no modifier)");
    println!("Press Ctrl+C to exit");
    println!();

    enable_raw_mode()?;

    loop {
        if event::poll(std::time::Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        break;
                    }
                    if key.code == KeyCode::Enter {
                        println!("Enter pressed: modifiers={:?}, bits={:?}, SHIFT={}",
                            key.modifiers,
                            key.modifiers.bits(),
                            key.modifiers.contains(KeyModifiers::SHIFT)
                        );
                    }
                }
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    println!("\nDone!");
    Ok(())
}
