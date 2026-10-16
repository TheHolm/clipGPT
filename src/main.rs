use clipboard::{ClipboardContext, ClipboardProvider};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use actually_beep::beep_with_hz_and_millis;

mod configfile;

#[derive(Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<Message>,
    keep_alive: Option<String>,
}

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OpenAIResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: MessageResponse,
}

#[derive(Deserialize)]
struct MessageResponse {
    content: String,
}

fn main() {
    let config = configfile::read_ini().unwrap_or_else(|error| {
        eprintln!("Failed to load config.ini: {}", error);
        std::process::exit(1);
    });

    let mut ctx: ClipboardContext = ClipboardProvider::new().expect("Failed to initialize clipboard");

    // Step 1: Grab clipboard content
    let input_text = ctx.get_contents().expect("Failed to get clipboard content");

    // Step 2: Prepare OpenAI API payload
    let prompt = format!("{}{}", config.prompt_text, input_text);
    let payload = OpenAIRequest {
        model: config.model.clone(),
        messages: vec![Message {
            role: "user".to_string(),
            content: prompt,
        }],
         keep_alive: Some("8h".to_string()),
    };

    // return;

    // Step 3: Send request to OpenAI API
    let client = Client::new();
    let response = client
        .post(&config.api_url)
        .header("Authorization", format!("Bearer {}", config.api_key))
        .json(&payload)
        .send()
        .expect("Failed to send request");

    // Parse the response safely
    let response_text = response.text().expect("Failed to read response text");
    let openai_response: OpenAIResponse = serde_json::from_str(&response_text)
        .expect(&format!("Failed to parse response: {}", response_text));

    // Step 4: Put response back to clipboard
    let pirate_text = &openai_response.choices[0].message.content;
    ctx.set_contents(pirate_text.clone().trim().to_string())
        .expect("Failed to set clipboard content");

    beep_with_hz_and_millis(config.beep_tone, config.beep_duration).unwrap();

    println!("Response copied to clipboard!");
}
