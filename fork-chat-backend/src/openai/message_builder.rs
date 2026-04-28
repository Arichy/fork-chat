use async_openai::types::responses::{EasyInputContent, EasyInputMessage, InputItem, Item, Role};
use sqlx::PgPool;

use crate::error::Result;
use crate::models::{Session, Turn};

pub async fn build_input_for_turn(
    db: &PgPool,
    _session: &Session,
    parent_turn_id: Option<uuid::Uuid>,
    new_user_content: &str,
) -> Result<Vec<InputItem>> {
    let turns = crate::db::get_path_to_turn(db, parent_turn_id).await?;

    let mut items: Vec<InputItem> = Vec::new();

    for turn in &turns {
        let turn_items = parse_turn_items(turn);
        if turn_items.is_empty() {
            let fallback = build_fallback_items(turn);
            items.extend(fallback);
        } else {
            items.extend(turn_items);
        }
    }

    items.push(InputItem::EasyMessage(EasyInputMessage {
        role: Role::User,
        content: EasyInputContent::Text(new_user_content.to_string()),
        ..Default::default()
    }));

    Ok(items)
}

fn parse_turn_items(turn: &Turn) -> Vec<InputItem> {
    let Some(arr) = turn.raw_items.as_array() else {
        return Vec::new();
    };

    if arr.is_empty() {
        return Vec::new();
    }

    let mut items = Vec::new();

    for value in arr {
        if let Ok(item) = serde_json::from_value::<Item>(value.clone()) {
            items.push(InputItem::Item(item));
            continue;
        }

        if let Ok(easy_msg) = serde_json::from_value::<EasyInputMessage>(value.clone()) {
            items.push(InputItem::EasyMessage(easy_msg));
        }
    }

    items
}

fn build_fallback_items(turn: &Turn) -> Vec<InputItem> {
    let mut items = Vec::new();

    if let Some(user_text) = &turn.user_text {
        items.push(InputItem::EasyMessage(EasyInputMessage {
            role: Role::User,
            content: EasyInputContent::Text(user_text.clone()),
            ..Default::default()
        }));
    }

    if let Some(assistant_text) = &turn.assistant_text {
        items.push(InputItem::EasyMessage(EasyInputMessage {
            role: Role::Assistant,
            content: EasyInputContent::Text(assistant_text.clone()),
            ..Default::default()
        }));
    }

    items
}

pub fn get_instructions(session: &Session) -> Option<&str> {
    session.system_prompt.as_deref()
}
