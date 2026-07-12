use gettextrs::{LocaleCategory, setlocale};
use std::sync::RwLock;

pub const LANGUAGE_SETTING_KEY: &str = "language";

static CURRENT_LANGUAGE: RwLock<Language> = RwLock::new(Language::English);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Japanese,
    Spanish,
}

impl Language {
    pub fn from_combo_id(id: &str) -> Self {
        match id {
            "ja" => Self::Japanese,
            "es" => Self::Spanish,
            _ => Self::English,
        }
    }

    pub fn from_locale(locale: &str) -> Self {
        let locale = locale.to_ascii_lowercase();

        if locale.contains("ja") {
            Self::Japanese
        } else if locale.contains("es") {
            Self::Spanish
        } else {
            Self::English
        }
    }

    pub fn from_saved_or_environment(saved_locale: Option<&str>) -> Self {
        saved_locale.map(Self::from_locale).unwrap_or_else(|| {
            std::env::var("LANG")
                .map(|locale| Self::from_locale(&locale))
                .unwrap_or(Self::English)
        })
    }

    pub fn combo_id(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Japanese => "ja",
            Self::Spanish => "es",
        }
    }

    pub fn locale(self) -> &'static str {
        match self {
            Self::English => "en_US.UTF-8",
            Self::Japanese => "ja_JP.UTF-8",
            Self::Spanish => "es_ES.UTF-8",
        }
    }
}

pub fn apply_language(language: Language) {
    setlocale(LocaleCategory::LcAll, language.locale());

    if let Ok(mut current_language) = CURRENT_LANGUAGE.write() {
        *current_language = language;
    }
}

pub fn translate(msgid: &str) -> String {
    let language = CURRENT_LANGUAGE
        .read()
        .map(|current_language| *current_language)
        .unwrap_or(Language::English);

    translate_for_language(language, msgid).to_string()
}

fn translate_for_language(language: Language, msgid: &str) -> &str {
    match language {
        Language::English => msgid,
        Language::Japanese => match msgid {
            "Dashboard" => "ダッシュボード",
            "Log Analysis" => "ログ分析",
            "Data Charts" => "データチャート",
            "Maintenance" => "整備記録",
            "CAN IDs" => "CAN ID",
            "IDs" => "ID",
            "Settings" => "設定",
            "Vehicle health" => "健康スコア",
            "Recalculate all" => "すべて再計算",
            "Hour" => "時間",
            "Day" => "日",
            "Week" => "週",
            "Month" => "月",
            "Year" => "年",
            "Previous period" => "前の期間",
            "Next period" => "次の期間",
            "Score trend" => "スコア推移",
            "Areas" => "領域別スコア",
            "Why the score changed" => "スコアが変化した理由",
            "Period data" => "対象期間データ",
            "Backfill / recalculation" => "バックフィル／再計算",
            "No recalculation result" => "再計算結果はありません",
            "Recalculate all health scores?" => "すべての健康スコアを再計算しますか？",
            "Raw logs will not be deleted or changed. Processing continues in the background." => {
                "生ログは削除・変更されません。処理はバックグラウンドで続行します。"
            }
            "Read-only database; recalculation is disabled" => {
                "データベースが読み取り専用のため再計算できません"
            }
            "Read-only" => "読み取り専用",
            "Recalculation in progress" => "再計算中",
            "Recalculation completed" => "再計算が完了しました",
            "Recalculation failed; retry is available" => "再計算に失敗しました。再試行できます",
            "Database error" => "データベースエラー",
            "No data" => "データなし",
            "No driving data in this period" => "この期間に走行データはありません",
            "Learning" => "学習中",
            "Insufficient data" => "データ不足",
            "Calculation failed" => "計算失敗",
            "Critical decline" => "重大な低下",
            "Attention" => "注意",
            "Healthy" => "正常",
            "Unavailable" => "評価不能",
            "vs previous" => "前回比",
            "Previous comparison unavailable" => "前回比は評価不能",
            "Confidence" => "判定信頼度",
            "Last updated" => "最終更新",
            "Valid sessions" => "有効セッション",
            "Learning time" => "学習時間",
            "Current score is provisional" => "現在のスコアは参考値",
            "Trips" => "走行数",
            "Evaluated time" => "評価時間",
            "Samples" => "サンプル数",
            "Data coverage" => "データカバレッジ",
            "Coverage" => "カバレッジ",
            "Completed" => "完了",
            "Processing" => "処理中",
            "Thermal" => "thermal（熱）",
            "Electrical" => "electrical（電気）",
            "Air / fuel" => "air_fuel（吸気・燃料）",
            "Running stability" => "running_stability（走行安定性）",
            "No significant change reason" => "顕著な変化理由はありません",
            "Impact" => "影響量",
            "Language" => "言語",
            "CAN ID Manager" => "CAN ID管理",
            "ID Manager" => "ID管理",
            "Review known definitions and promote unknown CAN IDs." => {
                "既知の定義を確認し、不明なCAN IDを登録します。"
            }
            "Review known definitions and promote unknown IDs." => {
                "既知の定義を確認し、不明なIDを登録します。"
            }
            "Known PIDs" => "既知のPID",
            "Known CAN IDs" => "既知のCAN ID",
            "Unknown PIDs" => "不明なPID",
            "Unknown CAN IDs" => "不明なCAN ID",
            "Known Signals" => "既知の信号",
            "Plot DuckDB log data by known PID or CAN ID definitions." => {
                "DuckDBのログデータを既知のPIDまたはCAN ID定義で表示します。"
            }
            "Select signals to overlay on the time axis." => {
                "時間軸に重ねて表示する信号を選択します。"
            }
            "Time Series" => "時系列",
            "No data loaded" => "データが読み込まれていません",
            "Compare" => "比較",
            "Absolute" => "実値",
            "Refresh" => "更新",
            "Compare scales each selected signal to 0-100% and keeps actual ranges in the legend." => {
                "比較表示では各信号を0-100%に正規化し、実値範囲は凡例に表示します。"
            }
            "No known PIDs" => "既知のPIDがありません",
            "No known CAN IDs" => "既知のCAN IDがありません",
            "No unknown PIDs" => "不明なPIDはありません",
            "No unknown CAN IDs" => "不明なCAN IDはありません",
            "No known signal definitions" => "既知の信号定義がありません",
            "Repository is unavailable" => "リポジトリを利用できません",
            "Select signals" => "信号を選択してください",
            "Failed to load" => "読み込みに失敗しました",
            "Save" => "保存",
            "Promote" => "登録",
            "Name" => "名前",
            "Formula" => "式",
            "No selected signal data" => "選択された信号データがありません",
            "normalized per signal" => "信号ごとに正規化",
            "absolute values" => "実値",
            "series" => "系列",
            "points" => "点",
            "now" => "最新",
            "Realtime vehicle telemetry" => "リアルタイム車両テレメトリー",
            "● LIVE" => "● ライブ",
            "ENGINE" => "エンジン",
            "Vehicle speed" => "車速",
            "Intake pressure" => "吸気圧",
            "Timing advance" => "点火時期",
            "Voltage" => "電圧",
            "LIVE RATIOS" => "リアルタイム比率",
            "TEMPERATURES" => "温度",
            "Coolant" => "冷却水",
            "Intake air" => "吸気",
            "Ambient" => "外気",
            "Engine oil" => "エンジンオイル",
            "Catalyst" => "触媒",
            "TRIP & DIAGNOSTICS" => "走行・診断情報",
            "MIL distance" => "警告灯点灯中の距離",
            "DTC cleared distance" => "故障コード消去後の距離",
            "Engine run time" => "エンジン経過時間",
            "Signal" => "信号",
            "Value" => "値",
            "Source" => "送信元",
            "CAN ID" => "CAN ID",
            "Payload" => "ペイロード",
            "Count" => "回数",
            "Engine load" => "エンジン負荷",
            "Throttle" => "スロットル",
            "Accelerator" => "アクセル",
            "Commanded throttle" => "指示スロットル",
            "Short fuel trim" => "短期燃調",
            "Long fuel trim" => "長期燃調",
            "Fuel level" => "燃料残量",
            "No frames" => "フレーム未受信",
            "Waiting for decoded CAN/PID values" => "CAN/PIDの解析値を待っています",
            "No unknown frames" => "不明なフレームはありません",
            "Some changes might require restart." => {
                "一部の変更を適用するには再起動が必要な場合があります。"
            }
            _ => msgid,
        },
        Language::Spanish => match msgid {
            "Dashboard" => "Tablero",
            "Log Analysis" => "Análisis de registros",
            "Data Charts" => "Gráficas de datos",
            "Maintenance" => "Mantenimiento",
            "CAN IDs" => "IDs CAN",
            "IDs" => "IDs",
            "Settings" => "Configuración",
            "Language" => "Idioma",
            "CAN ID Manager" => "Gestor de IDs CAN",
            "ID Manager" => "Gestor de IDs",
            "Review known definitions and promote unknown CAN IDs." => {
                "Revise definiciones conocidas y registre IDs CAN desconocidos."
            }
            "Review known definitions and promote unknown IDs." => {
                "Revise definiciones conocidas y registre IDs desconocidos."
            }
            "Known PIDs" => "PIDs conocidos",
            "Known CAN IDs" => "IDs CAN conocidos",
            "Unknown PIDs" => "PIDs desconocidos",
            "Unknown CAN IDs" => "IDs CAN desconocidos",
            "Known Signals" => "Señales conocidas",
            "Plot DuckDB log data by known PID or CAN ID definitions." => {
                "Grafique los registros de DuckDB con definiciones conocidas de PID o ID CAN."
            }
            "Select signals to overlay on the time axis." => {
                "Seleccione señales para superponerlas en el eje de tiempo."
            }
            "Time Series" => "Serie temporal",
            "No data loaded" => "No hay datos cargados",
            "Compare" => "Comparar",
            "Absolute" => "Absoluto",
            "Refresh" => "Actualizar",
            "Compare scales each selected signal to 0-100% and keeps actual ranges in the legend." => {
                "Comparar escala cada señal a 0-100% y conserva los rangos reales en la leyenda."
            }
            "No known PIDs" => "No hay PIDs conocidos",
            "No known CAN IDs" => "No hay IDs CAN conocidos",
            "No unknown PIDs" => "No hay PIDs desconocidos",
            "No unknown CAN IDs" => "No hay IDs CAN desconocidos",
            "No known signal definitions" => "No hay definiciones de señal conocidas",
            "Repository is unavailable" => "El repositorio no está disponible",
            "Select signals" => "Seleccione señales",
            "Failed to load" => "Error al cargar",
            "Save" => "Guardar",
            "Promote" => "Registrar",
            "Name" => "Nombre",
            "Formula" => "Fórmula",
            "No selected signal data" => "No hay datos para las señales seleccionadas",
            "normalized per signal" => "normalizado por señal",
            "absolute values" => "valores absolutos",
            "series" => "series",
            "points" => "puntos",
            "now" => "actual",
            "Realtime vehicle telemetry" => "Telemetría del vehículo en tiempo real",
            "● LIVE" => "● EN VIVO",
            "ENGINE" => "MOTOR",
            "Vehicle speed" => "Velocidad",
            "Intake pressure" => "Presión de admisión",
            "Timing advance" => "Avance de encendido",
            "Voltage" => "Voltaje",
            "LIVE RATIOS" => "PROPORCIONES EN VIVO",
            "TEMPERATURES" => "TEMPERATURAS",
            "Coolant" => "Refrigerante",
            "Intake air" => "Aire de admisión",
            "Ambient" => "Ambiente",
            "Engine oil" => "Aceite del motor",
            "Catalyst" => "Catalizador",
            "TRIP & DIAGNOSTICS" => "VIAJE Y DIAGNÓSTICO",
            "MIL distance" => "Distancia con MIL",
            "DTC cleared distance" => "Distancia desde borrado DTC",
            "Engine run time" => "Tiempo del motor",
            "Signal" => "Señal",
            "Value" => "Valor",
            "Source" => "Origen",
            "CAN ID" => "ID CAN",
            "Payload" => "Carga útil",
            "Count" => "Conteo",
            "Engine load" => "Carga del motor",
            "Throttle" => "Acelerador",
            "Accelerator" => "Pedal del acelerador",
            "Commanded throttle" => "Acelerador solicitado",
            "Short fuel trim" => "Ajuste corto de combustible",
            "Long fuel trim" => "Ajuste largo de combustible",
            "Fuel level" => "Nivel de combustible",
            "No frames" => "Sin tramas",
            "Waiting for decoded CAN/PID values" => "Esperando valores CAN/PID decodificados",
            "No unknown frames" => "No hay tramas desconocidas",
            "Some changes might require restart." => "Algunos cambios pueden requerir un reinicio.",
            _ => msgid,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_locales() {
        assert_eq!(Language::from_locale("ja_JP.UTF-8"), Language::Japanese);
        assert_eq!(Language::from_locale("es_ES.UTF-8"), Language::Spanish);
        assert_eq!(Language::from_locale("en_US.UTF-8"), Language::English);
    }

    #[test]
    fn uses_saved_locale_before_environment() {
        assert_eq!(
            Language::from_saved_or_environment(Some("ja_JP.UTF-8")),
            Language::Japanese
        );
    }

    #[test]
    fn translates_known_messages() {
        assert_eq!(
            translate_for_language(Language::Japanese, "Settings"),
            "設定"
        );
        assert_eq!(
            translate_for_language(Language::Spanish, "Language"),
            "Idioma"
        );
        assert_eq!(
            translate_for_language(Language::Japanese, "Data Charts"),
            "データチャート"
        );
        assert_eq!(
            translate_for_language(Language::Spanish, "Compare"),
            "Comparar"
        );
        assert_eq!(
            translate_for_language(Language::Japanese, "Unknown"),
            "Unknown"
        );
    }
}
