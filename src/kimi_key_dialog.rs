//! Native modal API-key editor. No key is displayed, logged, or put in app settings.
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::EM_LIMITTEXT;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::kimi_credentials::{self, Region, SavedKey};
use crate::localization::LanguageId;
use crate::native_interop::wide_str;

const SAVE: i32 = 1;
const CANCEL: i32 = 2;
const KEY: i32 = 1001;
const REGION: i32 = 1002;
const REMOVE: i32 = 1003;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Cancelled,
    Saved,
    Removed,
}

#[derive(Clone, Copy)]
struct Labels {
    menu: &'static str,
    title: &'static str,
    intro: &'static str,
    region: &'static str,
    china: &'static str,
    international: &'static str,
    key: &'static str,
    saved: &'static str,
    empty: &'static str,
    privacy: &'static str,
    save: &'static str,
    cancel: &'static str,
    remove: &'static str,
    confirm_remove: &'static str,
    invalid: &'static str,
    change_region: &'static str,
    storage_error: &'static str,
}

fn labels(language: LanguageId) -> Labels {
    match language {
        LanguageId::French => Labels {
            menu: "Configurer la clé API Kimi Code…",
            title: "Kimi Code — Clé API",
            intro: "Utilisez une clé de la console Kimi Code, pas du service Kimi Open Platform.",
            region: "Région :",
            china: "Chine (kimi.com)",
            international: "International (kimi.ai)",
            key: "Clé API (saisie masquée) :",
            saved: "Une clé est enregistrée. Laissez ce champ vide pour la conserver ou saisissez une nouvelle clé.",
            empty: "Collez votre clé API. Enregistrer active Kimi Code et actualise ses quotas.",
            privacy: "Stockage : Gestionnaire d’identifiants Windows, jamais settings.json.",
            save: "Enregistrer",
            cancel: "Annuler",
            remove: "Supprimer la clé",
            confirm_remove: "Supprimer la clé enregistrée par cette application ? Les identifiants du CLI et les variables d’environnement ne seront pas modifiés.",
            invalid: "Saisissez une clé API Kimi Code valide, sans espace ni retour à la ligne.",
            change_region: "Pour changer de région, saisissez une clé émise par la nouvelle région.",
            storage_error: "Impossible de lire ou d’enregistrer la clé dans le Gestionnaire d’identifiants Windows.",
        },
        LanguageId::SimplifiedChinese => Labels {
            menu: "设置 Kimi Code API Key…",
            title: "Kimi Code — API Key",
            intro: "请使用 Kimi Code 控制台创建的密钥，而不是 Kimi 开放平台密钥。",
            region: "区域：",
            china: "中国（kimi.com）",
            international: "国际（kimi.ai）",
            key: "API Key（隐藏输入）：",
            saved: "已保存密钥。留空可保留原密钥，或输入新密钥替换。",
            empty: "粘贴 API Key。保存后会启用 Kimi Code 并刷新额度。",
            privacy: "密钥保存在 Windows 凭据管理器，不会写入 settings.json。",
            save: "保存",
            cancel: "取消",
            remove: "删除密钥",
            confirm_remove: "删除本应用保存的密钥？不会更改 CLI 凭据或环境变量。",
            invalid: "请输入有效的 Kimi Code API Key，不能包含空格或换行。",
            change_region: "切换区域时，请输入对应区域签发的新密钥。",
            storage_error: "无法在 Windows 凭据管理器中读取或保存密钥。",
        },
        _ => Labels {
            menu: "Configure Kimi Code API key…",
            title: "Kimi Code — API key",
            intro: "Use a key from the Kimi Code console, not Kimi Open Platform.",
            region: "Region:",
            china: "China (kimi.com)",
            international: "International (kimi.ai)",
            key: "API key (hidden input):",
            saved: "A key is saved. Leave this field empty to keep it, or enter a replacement key.",
            empty: "Paste your API key. Saving enables Kimi Code and refreshes its quotas.",
            privacy: "Stored in Windows Credential Manager, never in settings.json.",
            save: "Save",
            cancel: "Cancel",
            remove: "Remove key",
            confirm_remove: "Remove the key saved by this app? CLI credentials and environment variables will not be changed.",
            invalid: "Enter a valid Kimi Code API key without spaces or line breaks.",
            change_region: "To change regions, enter a key issued for the new region.",
            storage_error: "Unable to read or save the key in Windows Credential Manager.",
        },
    }
}

pub fn menu_label(language: LanguageId) -> &'static str {
    labels(language).menu
}

struct State {
    labels: Labels,
    saved: Option<SavedKey>,
    outcome: Outcome,
}

pub fn show(owner: HWND, language: LanguageId) -> Result<Outcome, String> {
    let text = labels(language);
    let mut state = State {
        labels: text,
        saved: kimi_credentials::load().map_err(|_| text.storage_error.to_string())?,
        outcome: Outcome::Cancelled,
    };
    let template = template();
    let result = unsafe {
        let instance = GetModuleHandleW(None).map_err(|_| text.storage_error.to_string())?;
        DialogBoxIndirectParamW(
            instance,
            template.as_ptr().cast(),
            owner,
            Some(dialog_proc),
            LPARAM(&mut state as *mut State as isize),
        )
    };
    if result == -1 {
        Err("Unable to open the Kimi Code API-key dialog.".into())
    } else {
        Ok(state.outcome)
    }
}

// Standard DLGTEMPLATE, DWORD-aligned, with no resource-file dependency.
fn template() -> Vec<u32> {
    let style = WS_POPUP.0
        | WS_CAPTION.0
        | WS_SYSMENU.0
        | (DS_MODALFRAME | DS_SETFONT | DS_CENTER | DS_SETFOREGROUND) as u32;
    let mut words = vec![
        style as u16,
        (style >> 16) as u16,
        0,
        0,
        0,
        0,
        0,
        360,
        204,
        0,
        0,
        0,
        9,
    ]; // menu, class, title, font point size
    words.extend("Segoe UI".encode_utf16().chain(Some(0)));
    if words.len() % 2 != 0 {
        words.push(0);
    }
    words
        .chunks_exact(2)
        .map(|pair| u32::from(pair[0]) | (u32::from(pair[1]) << 16))
        .collect()
}

#[allow(clippy::too_many_arguments)]
unsafe fn control(
    dialog: HWND,
    class: PCWSTR,
    text: &str,
    style: u32,
    id: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
) -> windows::core::Result<HWND> {
    let text = wide_str(text);
    let mut rect = RECT {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    };
    MapDialogRect(dialog, &mut rect)?;
    let window = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        PCWSTR(text.as_ptr()),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
        dialog,
        HMENU(id as isize as *mut _),
        GetModuleHandleW(None)?,
        None,
    )?;
    let font = SendMessageW(dialog, WM_GETFONT, WPARAM(0), LPARAM(0));
    SendMessageW(window, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    Ok(window)
}

unsafe fn initialize(dialog: HWND, state: &State) -> windows::core::Result<()> {
    let text = state.labels;
    let title = wide_str(text.title);
    SetWindowTextW(dialog, PCWSTR(title.as_ptr()))?;
    control(dialog, w!("STATIC"), text.intro, 0, -1, 12, 10, 336, 28)?;
    control(dialog, w!("STATIC"), text.region, 0, -1, 12, 44, 70, 12)?;
    let region = control(
        dialog,
        w!("COMBOBOX"),
        "",
        CBS_DROPDOWNLIST as u32 | WS_TABSTOP.0 | WS_VSCROLL.0,
        REGION,
        90,
        42,
        258,
        85,
    )?;
    for label in [text.china, text.international] {
        let label = wide_str(label);
        SendMessageW(
            region,
            CB_ADDSTRING,
            WPARAM(0),
            LPARAM(label.as_ptr() as isize),
        );
    }
    let selected = state.saved.as_ref().map_or(Region::China, |key| key.region);
    SendMessageW(
        region,
        CB_SETCURSEL,
        WPARAM(usize::from(selected == Region::International)),
        LPARAM(0),
    );
    control(dialog, w!("STATIC"), text.key, 0, -1, 12, 73, 336, 12)?;
    let edit = control(
        dialog,
        w!("EDIT"),
        "",
        WS_TABSTOP.0 | WS_BORDER.0 | (ES_PASSWORD | ES_AUTOHSCROLL) as u32,
        KEY,
        12,
        89,
        336,
        18,
    )?;
    SendMessageW(
        edit,
        EM_LIMITTEXT,
        WPARAM(kimi_credentials::MAX_KEY_LENGTH + 1),
        LPARAM(0),
    );
    control(
        dialog,
        w!("STATIC"),
        if state.saved.is_some() {
            text.saved
        } else {
            text.empty
        },
        0,
        -1,
        12,
        114,
        336,
        27,
    )?;
    control(dialog, w!("STATIC"), text.privacy, 0, -1, 12, 145, 336, 18)?;
    let remove = control(
        dialog,
        w!("BUTTON"),
        text.remove,
        WS_TABSTOP.0,
        REMOVE,
        12,
        174,
        88,
        18,
    )?;
    let _ = EnableWindow(remove, state.saved.is_some());
    control(
        dialog,
        w!("BUTTON"),
        text.save,
        WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
        SAVE,
        195,
        174,
        80,
        18,
    )?;
    control(
        dialog,
        w!("BUTTON"),
        text.cancel,
        WS_TABSTOP.0,
        CANCEL,
        282,
        174,
        66,
        18,
    )?;
    let _ = SetFocus(edit);
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum InvalidInput {
    Key,
    Region,
}

fn submitted_key(
    input: String,
    region: Region,
    saved: Option<&SavedKey>,
) -> Result<SavedKey, InvalidInput> {
    let value = input.trim();
    if value.is_empty() {
        let saved = saved.ok_or(InvalidInput::Key)?;
        if saved.region != region {
            return Err(InvalidInput::Region);
        }
        return Ok(SavedKey {
            api_key: saved.api_key.clone(),
            region,
        });
    }
    if !kimi_credentials::valid_key(value) {
        return Err(InvalidInput::Key);
    }
    Ok(SavedKey {
        api_key: value.to_string(),
        region,
    })
}

unsafe fn message(
    dialog: HWND,
    text: &str,
    title: &str,
    flags: MESSAGEBOX_STYLE,
) -> MESSAGEBOX_RESULT {
    let text = wide_str(text);
    let title = wide_str(title);
    MessageBoxW(dialog, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), flags)
}

unsafe fn close(dialog: HWND) {
    let _ = SetDlgItemTextW(dialog, KEY, w!(""));
    let _ = EndDialog(dialog, 0);
}

unsafe fn submit(dialog: HWND, state: &mut State) {
    let Ok(edit) = GetDlgItem(dialog, KEY) else {
        return;
    };
    let Ok(region_control) = GetDlgItem(dialog, REGION) else {
        return;
    };
    let region = match SendMessageW(region_control, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0 {
        0 => Region::China,
        1 => Region::International,
        _ => return,
    };
    let length = GetWindowTextLengthW(edit).max(0) as usize;
    let mut buffer = vec![0u16; length.min(kimi_credentials::MAX_KEY_LENGTH + 1) + 1];
    let copied = GetWindowTextW(edit, &mut buffer).max(0) as usize;
    let input = String::from_utf16(&buffer[..copied]);
    buffer.fill(0);
    let result = input
        .map_err(|_| InvalidInput::Key)
        .and_then(|input| submitted_key(input, region, state.saved.as_ref()));
    let key = match result {
        Ok(key) => key,
        Err(error) => {
            let text = if error == InvalidInput::Region {
                state.labels.change_region
            } else {
                state.labels.invalid
            };
            message(dialog, text, state.labels.title, MB_OK | MB_ICONWARNING);
            return;
        }
    };
    if kimi_credentials::save(&key).is_err() {
        message(
            dialog,
            state.labels.storage_error,
            state.labels.title,
            MB_OK | MB_ICONERROR,
        );
        return;
    }
    state.outcome = Outcome::Saved;
    close(dialog);
}

unsafe extern "system" fn dialog_proc(
    dialog: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    if msg == WM_INITDIALOG {
        SetWindowLongPtrW(dialog, GWLP_USERDATA, lparam.0);
        let state = &*(lparam.0 as *const State);
        if initialize(dialog, state).is_err() {
            let _ = EndDialog(dialog, -1);
        }
        return 0; // initialize set focus explicitly
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(dialog, GWLP_USERDATA, 0);
        return 0;
    }
    let pointer = GetWindowLongPtrW(dialog, GWLP_USERDATA) as *mut State;
    if pointer.is_null() {
        return 0;
    }
    match msg {
        WM_CLOSE => {
            close(dialog);
            1
        }
        WM_COMMAND => match wparam.0 as u16 as i32 {
            SAVE => {
                submit(dialog, &mut *pointer);
                1
            }
            CANCEL => {
                close(dialog);
                1
            }
            REMOVE => {
                // Native calls can re-enter the dialog procedure. Do not hold
                // a mutable state reference while confirmation messages run.
                let labels = (*pointer).labels;
                if message(
                    dialog,
                    labels.confirm_remove,
                    labels.title,
                    MB_YESNO | MB_ICONWARNING,
                ) == IDYES
                {
                    if kimi_credentials::remove().is_ok() {
                        (*pointer).outcome = Outcome::Removed;
                        close(dialog);
                    } else {
                        message(
                            dialog,
                            labels.storage_error,
                            labels.title,
                            MB_OK | MB_ICONERROR,
                        );
                    }
                }
                1
            }
            _ => 0,
        },
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_input_preserves_key_but_cannot_forward_it_to_another_region() {
        let saved = SavedKey {
            api_key: "sk-kimi-fixture".into(),
            region: Region::China,
        };
        let kept = submitted_key("".into(), Region::China, Some(&saved)).unwrap();
        assert_eq!(kept.api_key, saved.api_key);
        assert!(matches!(
            submitted_key("".into(), Region::International, Some(&saved)),
            Err(InvalidInput::Region)
        ));
        assert!(matches!(
            submitted_key("".into(), Region::China, None),
            Err(InvalidInput::Key)
        ));
        let replaced =
            submitted_key(" sk-kimi-new ".into(), Region::International, Some(&saved)).unwrap();
        assert_eq!(replaced.api_key, "sk-kimi-new");
        assert_eq!(replaced.region, Region::International);
    }

    #[test]
    fn native_editor_has_masked_empty_input_and_both_regions() {
        unsafe {
            // Modeless and hidden: inspect native controls without showing a UI,
            // calling the API, or writing to the real credential target.
            let mut state = State {
                labels: labels(LanguageId::French),
                saved: Some(SavedKey {
                    api_key: "sk-kimi-fixture".into(),
                    region: Region::International,
                }),
                outcome: Outcome::Cancelled,
            };
            let template = template();
            let dialog = CreateDialogIndirectParamW(
                GetModuleHandleW(None).unwrap(),
                template.as_ptr().cast(),
                None,
                Some(dialog_proc),
                LPARAM(&mut state as *mut State as isize),
            )
            .unwrap();
            let edit = GetDlgItem(dialog, KEY).unwrap();
            assert_ne!(GetWindowLongW(edit, GWL_STYLE) & ES_PASSWORD, 0);
            assert_eq!(GetWindowTextLengthW(edit), 0);
            let region = GetDlgItem(dialog, REGION).unwrap();
            assert_eq!(SendMessageW(region, CB_GETCOUNT, WPARAM(0), LPARAM(0)).0, 2);
            assert_eq!(
                SendMessageW(region, CB_GETCURSEL, WPARAM(0), LPARAM(0)).0,
                1
            );
            assert!(!IsWindowVisible(dialog).as_bool());
            DestroyWindow(dialog).unwrap();
            assert_eq!(state.outcome, Outcome::Cancelled);
        }
    }
}
