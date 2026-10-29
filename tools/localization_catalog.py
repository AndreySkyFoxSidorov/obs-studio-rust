#!/usr/bin/env python3
"""Merge supplied OBS localization into one user-editable UTF-8/BOM CSV.

No translation service, network, application execution or build is involved.
Existing CSV cells, including deliberate blanks, are authoritative and preserved.
Only the explicit --complete-app operation fills blank application cells from
the reviewed dictionary; every nonempty cell remains authoritative in that mode.
"""
from __future__ import annotations
import argparse
from collections import Counter
import csv
import hashlib
import io
import json
from pathlib import Path
import re
import sys
import tempfile
import unittest
import ui_chrome_audit as rust_lex

ROOT = Path(__file__).resolve().parents[1]
CSV = ROOT / "localization/translations.csv"
REQUIRED = ["en-US", "es-ES", "ru-RU", "uk-UA", "de-DE"]
OBS = ROOT / "doc/obs-studio"
ASSIGNMENT = re.compile(r'^([^=\s]+)\s*=\s*"((?:\\.|[^"\\])*)"')
RUST_STRING = re.compile(r'"(?:\\.|[^"\\])*"')
INI_WARNINGS = []

# Assistant-authored translations for common application labels and dialogs.
# They have no external/native-linguist review; this is not linguistic acceptance.
# Columns are key, English, Spanish, Russian, Ukrainian, German. The runtime
# consumes only translations.csv; these seeds never overwrite an existing cell.
APP = """Hotkeys.WindowsLimits	Ctrl, Alt and Shift are supported. Windows-key combinations and punctuation are unavailable.	Se admiten Ctrl, Alt y Mayús. Las combinaciones con la tecla Windows y los signos de puntuación no están disponibles.	Поддерживаются Ctrl, Alt и Shift. Сочетания с клавишей Windows и знаки пунктуации недоступны.	Підтримуються Ctrl, Alt і Shift. Поєднання з клавішею Windows і знаки пунктуації недоступні.	Ctrl, Alt und Shift werden unterstützt. Windows-Tastenkombinationen und Satzzeichen sind nicht verfügbar.
Hotkeys.GlobalUnsupported	Global shortcuts support letters, digits, navigation and function keys; punctuation is unavailable.	Los atajos globales admiten letras, dígitos, navegación y teclas de función; los signos de puntuación no están disponibles.	Глобальные сочетания поддерживают буквы, цифры, клавиши навигации и функциональные клавиши; знаки пунктуации недоступны.	Глобальні сполучення підтримують літери, цифри, клавіші навігації та функціональні клавіші; знаки пунктуації недоступні.	Globale Tastenkürzel unterstützen Buchstaben, Ziffern, Navigations- und Funktionstasten; Satzzeichen sind nicht verfügbar.
Hotkeys.AudioUnsupported	Source mute shortcuts use the supported Windows global keys; punctuation is unavailable.	Los atajos de silencio de la fuente usan las teclas globales admitidas de Windows; los signos de puntuación no están disponibles.	Сочетания отключения звука источника используют поддерживаемые глобальные клавиши Windows; знаки пунктуации недоступны.	Сполучення вимкнення звуку джерела використовують підтримувані глобальні клавіші Windows; знаки пунктуації недоступні.	Stummschaltungskürzel für Quellen verwenden unterstützte globale Windows-Tasten; Satzzeichen sind nicht verfügbar.
Hotkeys.PlaybackUnsupported	Source playback shortcuts use the supported Windows global keys; punctuation is unavailable.	Los atajos de reproducción de la fuente usan las teclas globales admitidas de Windows; los signos de puntuación no están disponibles.	Сочетания воспроизведения источника используют поддерживаемые глобальные клавиши Windows; знаки пунктуации недоступны.	Сполучення відтворення джерела використовують підтримувані глобальні клавіші Windows; знаки пунктуації недоступні.	Wiedergabekürzel für Quellen verwenden unterstützte globale Windows-Tasten; Satzzeichen sind nicht verfügbar.
Hotkeys.ObsUnsupported	This OBS source hotkey key is outside the exact Windows subset; keypad, modifier-only and punctuation keys are not converted	Esta tecla de atajo de fuente OBS está fuera del subconjunto exacto de Windows; no se convierten teclas numéricas, modificadores solos ni signos de puntuación	Эта клавиша сочетания источника OBS не входит в поддерживаемый набор Windows; цифровой блок, отдельные модификаторы и знаки пунктуации не преобразуются	Ця клавіша сполучення джерела OBS не входить до підтримуваного набору Windows; цифровий блок, окремі модифікатори та знаки пунктуації не перетворюються	Diese OBS-Quellen-Taste liegt außerhalb der unterstützten Windows-Auswahl; Ziffernblock, einzelne Modifikatortasten und Satzzeichen werden nicht umgewandelt
ImageMask.GifHint	PNG, JPEG, BMP, TGA, WebP or animated GIF. Changes reload automatically when dimensions stay the same. Empty path bypasses the filter.	PNG, JPEG, BMP, TGA, WebP o GIF animado. Los cambios se recargan automáticamente si conservan las dimensiones. Una ruta vacía omite el filtro.	PNG, JPEG, BMP, TGA, WebP или анимированный GIF. Изменения перезагружаются автоматически при сохранении размеров. Пустой путь отключает фильтр.	PNG, JPEG, BMP, TGA, WebP або анімований GIF. Зміни перезавантажуються автоматично, якщо розміри не змінено. Порожній шлях вимикає фільтр.	PNG, JPEG, BMP, TGA, WebP oder animiertes GIF. Änderungen werden bei gleichen Abmessungen automatisch neu geladen. Ein leerer Pfad umgeht den Filter.
ApplicationAudio.PersistentWindow	Persistent window	Ventana persistente	Постоянный выбор окна	Сталий вибір вікна	Dauerhafte Fensterauswahl
ApplicationAudio.WindowReconnectHint	Search for the window again after it closes or the application restarts.	Busca la ventana de nuevo tras cerrarse o reiniciarse la aplicación.	Повторный поиск окна после закрытия или перезапуска приложения.	Повторний пошук вікна після закриття або перезапуску програми.	Das Fenster nach dem Schließen oder Neustart der Anwendung erneut suchen.
Plugins\tPlugins\tComplementos\tПлагины\tПлагіни\tErweiterungen
SearchModules\tSearch modules\tBuscar módulos\tПоиск модулей\tПошук модулів\tModule suchen
NoModulesMatch\tNo modules match this search.\tNingún módulo coincide con la búsqueda.\tПо этому запросу модули не найдены.\tЗа цим запитом модулі не знайдено.\tKeine Module entsprechen dieser Suche.
AudioFilters\tAudio Filters\tFiltros de audio\tАудиофильтры\tАудіофільтри\tAudiofilter
SceneNotes\tScene Notes\tNotas de escena\tЗаметки сцены\tНотатки сцени\tSzenennotizen
AddColor\tAdd Color Source\tAñadir fuente de color\tДобавить источник цвета\tДодати джерело кольору\tFarbquelle hinzufügen
AddImage\tAdd Image Source\tAñadir fuente de imagen\tДобавить изображение\tДодати зображення\tBildquelle hinzufügen
AddDisplay\tAdd Display Capture\tAñadir captura de pantalla\tДобавить захват экрана\tДодати захоплення екрана\tBildschirmaufnahme hinzufügen
DisplayProperties\tProperties: Display Capture\tPropiedades: captura de pantalla\tСвойства: захват экрана\tВластивості: захоплення екрана\tEigenschaften: Bildschirmaufnahme
AudioProperties\tProperties: Audio Capture\tPropiedades: captura de audio\tСвойства: захват звука\tВластивості: захоплення звуку\tEigenschaften: Audioaufnahme
AddAudioInput\tAdd Audio Input Capture\tAñadir captura de entrada de audio\tДобавить захват входного аудиопотока\tДодати захоплення вхідного аудіопотоку\tAudioeingabeaufnahme hinzufügen
AddAudioOutput\tAdd Audio Output Capture\tAñadir captura de salida de audio\tДобавить захват выходного аудиопотока\tДодати захоплення вихідного аудіопотоку\tAudioausgabeaufnahme hinzufügen
RenameSource\tRename Source\tCambiar nombre de la fuente\tПереименовать источник\tПерейменувати джерело\tQuelle umbenennen
ColorProperties\tProperties: Color\tPropiedades: color\tСвойства: цвет\tВластивості: колір\tEigenschaften: Farbe
ImageProperties\tProperties: Image\tPropiedades: imagen\tСвойства: изображение\tВластивості: зображення\tEigenschaften: Bild
VideoSettings\tSettings — Video\tAjustes — Vídeo\tНастройки — Видео\tНалаштування — Відео\tEinstellungen — Video
AudioSettings\tSettings — Audio\tAjustes — Audio\tНастройки — Аудио\tНалаштування — Аудіо\tEinstellungen — Audio
NewProfile\tNew Profile\tNuevo perfil\tНовый профиль\tНовий профіль\tNeues Profil
DuplicateProfile\tDuplicate Profile\tDuplicar perfil\tДублировать профиль\tДублювати профіль\tProfil duplizieren
RenameProfile\tRename Profile\tCambiar nombre del perfil\tПереименовать профиль\tПерейменувати профіль\tProfil umbenennen
RemoveProfile\tRemove Profile\tEliminar perfil\tУдалить профиль\tВидалити профіль\tProfil entfernen
AddQuickTransition\tAdd Quick Transition\tAñadir transición rápida\tДобавить быстрый переход\tДодати швидкий перехід\tSchnellübergang hinzufügen
EditQuickTransition\tEdit Quick Transition\tEditar transición rápida\tИзменить быстрый переход\tЗмінити швидкий перехід\tSchnellübergang bearbeiten
SceneTransitionOverride\tScene Transition Override\tTransición específica de la escena\tПереход для отдельной сцены\tПерехід для окремої сцени\tSzenenspezifischer Übergang
AddTransitionPreset\tAdd Transition Preset\tAñadir preajuste de transición\tДобавить предустановку перехода\tДодати набір параметрів переходу\tÜbergangsvorgabe hinzufügen
TransitionPresetProperties\tTransition Preset Properties\tPropiedades del preajuste de transición\tСвойства предустановки перехода\tВластивості набору параметрів переходу\tEigenschaften der Übergangsvorgabe
RemoveTransitionPreset\tRemove Transition Preset\tEliminar preajuste de transición\tУдалить предустановку перехода\tВидалити набір параметрів переходу\tÜbergangsvorgabe entfernen
About\tAbout SkyLive\tAcerca de SkyLive\tО программе SkyLive\tПро SkyLive\tÜber SkyLive
ImportObs\tImport OBS Scene Collection\tImportar colección de escenas de OBS\tИмпорт коллекции сцен OBS\tІмпорт колекції сцен OBS\tOBS-Szenensammlung importieren
ExportObs\tExport OBS Scene Collection\tExportar colección de escenas de OBS\tЭкспорт коллекции сцен OBS\tЕкспорт колекції сцен OBS\tOBS-Szenensammlung exportieren
NoProviders\tNo providers available\tNo hay proveedores disponibles\tНет доступных модулей\tНемає доступних модулів\tKeine Anbieter verfügbar
ReloadImage\tReload Image\tRecargar imagen\tПерезагрузить изображение\tПерезавантажити зображення\tBild neu laden
ReplaceImage\tReplace Image\tReemplazar imagen\tЗаменить изображение\tЗамінити зображення\tBild ersetzen
ReloadTranslations\tReload Translations\tRecargar traducciones\tПерезагрузить переводы\tПерезавантажити переклади\tÜbersetzungen neu laden
Language\tLanguage\tIdioma\tЯзык\tМова\tSprache
Enabled\tEnabled\tActivado\tВключено\tУвімкнено\tAktiviert
Disabled\tDisabled\tDesactivado\tОтключено\tВимкнено\tDeaktiviert
RustPortAvailable\tRust port available\tVersión en Rust disponible\tВерсия на Rust доступна\tВерсія мовою Rust доступна\tRust-Version verfügbar
ReferenceOnly\tReference only · not ported\tSolo referencia · sin adaptar\tТолько исходники · не перенесён\tЛише вихідний код · не перенесено\tNur Referenz · noch nicht portiert
SourceTemplate\tSource: {name}\tFuente: {name}\tИсточник: {name}\tДжерело: {name}\tQuelle: {name}
SceneTemplate\tScene: {name}\tEscena: {name}\tСцена: {name}\tСцена: {name}\tSzene: {name}
RemoveFilter\tRemove Filter\tEliminar filtro\tУдалить фильтр\tВидалити фільтр\tFilter entfernen
FilterUp\tMove Filter Up\tSubir filtro\tПереместить фильтр вверх\tПеремістити фільтр угору\tFilter nach oben verschieben
FilterDown\tMove Filter Down\tBajar filtro\tПереместить фильтр вниз\tПеремістити фільтр униз\tFilter nach unten verschieben
AddPan\tAdd Audio Pan Filter\tAñadir filtro de panorama de audio\tДобавить фильтр панорамирования\tДодати фільтр панорамування\tAudiopanorama-Filter hinzufügen
NoAudioFilters\tNo audio filters.\tNo hay filtros de audio.\tАудиофильтры отсутствуют.\tАудіофільтри відсутні.\tKeine Audiofilter.
InvertLeft\tInvert (Left)\tInvertir (izquierda)\tИнвертировать (левый)\tІнвертувати (лівий)\tInvertieren (links)
InvertRight\tInvert (Right)\tInvertir (derecha)\tИнвертировать (правый)\tІнвертувати (правий)\tInvertieren (rechts)
PanLeft\tPan (Left), -100 to 100%\tPanorama (izquierda), de -100 a 100 %\tПанорама (левый), от -100 до 100%\tПанорама (лівий), від -100 до 100%\tPanorama (links), -100 bis 100 %
PanRight\tPan (Right), -100 to 100%\tPanorama (derecha), de -100 a 100 %\tПанорама (правый), от -100 до 100%\tПанорама (правий), від -100 до 100%\tPanorama (rechts), -100 bis 100 %
GainLeft\tGain (Left), -80 to 30 dB\tGanancia (izquierda), de -80 a 30 dB\tУсиление (левый), от -80 до 30 дБ\tПідсилення (лівий), від -80 до 30 дБ\tVerstärkung (links), -80 bis 30 dB
GainRight\tGain (Right), -80 to 30 dB\tGanancia (derecha), de -80 a 30 dB\tУсиление (правый), от -80 до 30 дБ\tПідсилення (правий), від -80 до 30 дБ\tVerstärkung (rechts), -80 bis 30 dB
NativeDevices\tNative Devices\tDispositivos del sistema\tСистемные устройства\tСистемні пристрої\tSystemgeräte
CaptureCursor\tCapture Cursor\tCapturar cursor\tЗахватывать курсор\tЗахоплювати курсор\tMauszeiger aufnehmen
UseDeviceTiming\tUse Device Timing\tUsar reloj del dispositivo\tИспользовать время устройства\tВикористовувати час пристрою\tGerätezeit verwenden
ExcludeOwnAudio\tExclude Own Audio\tExcluir el audio propio\tИсключить собственный звук\tВиключити власний звук\tEigenes Audio ausschließen
StartAudioProgram\tStart Program Audio\tIniciar audio del programa\tЗапустить звук программы\tЗапустити звук програми\tProgrammaudio starten
StopAudioProgram\tStop Program Audio\tDetener audio del programa\tОстановить звук программы\tЗупинити звук програми\tProgrammaudio stoppen
AudioPanFilter\tAudio Pan Filter\tFiltro de panorama de audio\tФильтр панорамирования звука\tФільтр панорамування звуку\tAudiopanorama-Filter
NoSelection\tNo selection\tSin selección\tНичего не выбрано\tНічого не вибрано\tKeine Auswahl
FilePath\tFile path\tRuta del archivo\tПуть к файлу\tШлях до файлу\tDateipfad
Reload\tReload\tRecargar\tПерезагрузить\tПерезавантажити\tNeu laden
Preview\tPreview\tVista previa\tПредпросмотр\tПопередній перегляд\tVorschau
Program\tProgram\tPrograma\tПрограмма\tПрограма\tProgramm
ResetLayout\tReset Layout\tRestablecer disposición\tСбросить расположение\tСкинути розташування\tAnordnung zurücksetzen
PlainNotesScope\tPlain-text notes only. Formatted notes, linked files, auto-scroll and hotkeys are pending.\tSolo notas de texto. El formato, los archivos vinculados, el desplazamiento automático y los atajos están pendientes.\tТолько текстовые заметки. Форматирование, связанные файлы, автопрокрутка и горячие клавиши ещё не реализованы.\tЛише текстові нотатки. Форматування, пов’язані файли, автопрокручування та гарячі клавіші ще не реалізовані.\tNur Textnotizen. Formatierung, verknüpfte Dateien, automatischer Bildlauf und Tastenkürzel sind noch nicht implementiert.
NotesApplyHint\tApply commits this scene's note; Cancel discards the draft.\tAplicar guarda la nota de esta escena; Cancelar descarta el borrador.\t«Применить» сохраняет заметку сцены; «Отмена» удаляет черновик.\t«Застосувати» зберігає нотатку сцени; «Скасувати» відкидає чернетку.\tAnwenden speichert die Szenennotiz; Abbrechen verwirft den Entwurf.
ModulesOffDefault\tThird-party modules are off by default. Only available Rust ports can be enabled; OBS plugin binaries cannot be loaded here.\tLos módulos externos están desactivados de forma predeterminada. Solo se pueden activar las versiones en Rust disponibles; aquí no se pueden cargar binarios de complementos de OBS.\tСторонние модули по умолчанию отключены. Включить можно только доступные версии на Rust; бинарные плагины OBS здесь не загружаются.\tСторонні модулі типово вимкнені. Увімкнути можна лише доступні версії мовою Rust; бінарні плагіни OBS тут не завантажуються.\tDrittanbietermodule sind standardmäßig deaktiviert. Nur verfügbare Rust-Versionen können aktiviert werden; OBS-Plugindateien lassen sich hier nicht laden.
CaptureCursorLabel\tCapture cursor\tCapturar cursor\tЗахватывать курсор\tЗахоплювати курсор\tMauszeiger aufnehmen
CommonUseDeviceTiming\tUse device timing\tUsar reloj del dispositivo\tИспользовать время устройства\tВикористовувати час пристрою\tGerätezeit verwenden
ExcludeApplicationAudio\tExclude this application's audio\tExcluir el audio de esta aplicación\tИсключить звук этого приложения\tВиключити звук цієї програми\tAudio dieser Anwendung ausschließen
RefreshDisplays\tRefresh Displays\tActualizar pantallas\tОбновить список экранов\tОновити список екранів\tBildschirme aktualisieren
ListNativeDevices\tList Native Devices\tMostrar dispositivos del sistema\tПоказать системные устройства\tПоказати системні пристрої\tSystemgeräte auflisten
SelectDisplay\tSelect display\tSeleccionar pantalla\tВыберите экран\tВиберіть екран\tBildschirm auswählen
NoDisplays\tNo displays are available.\tNo hay pantallas disponibles.\tНет доступных экранов.\tНемає доступних екранів.\tKeine Bildschirme verfügbar.
EnumeratingDisplays\tEnumerating displays…\tBuscando pantallas…\tПолучение списка экранов…\tОтримання списку екранів…\tBildschirme werden ermittelt…
EnumeratingAudioDevices\tEnumerating audio devices…\tBuscando dispositivos de audio…\tПолучение списка аудиоустройств…\tОтримання списку аудіопристроїв…\tAudiogeräte werden ermittelt…
ChooseListedDevice\tChoose listed device\tElegir dispositivo de la lista\tВыберите устройство из списка\tВиберіть пристрій зі списку\tGerät aus der Liste auswählen
DisabledNotes\tModule disabled · notes retained. Enable Scene Notes Dock in Tools > Plugins to edit.\tMódulo desactivado · notas conservadas. Active Scene Notes Dock en Herramientas > Complementos para editar.\tМодуль отключён · заметки сохранены. Для редактирования включите Scene Notes Dock в меню «Инструменты > Плагины».\tМодуль вимкнено · нотатки збережено. Для редагування увімкніть Scene Notes Dock у меню «Інструменти > Плагіни».\tModul deaktiviert · Notizen bleiben erhalten. Zum Bearbeiten Scene Notes Dock unter Werkzeuge > Erweiterungen aktivieren.
DisabledFilters\tModule disabled · settings retained. Enable it in Tools > Plugins to edit these properties.\tMódulo desactivado · ajustes conservados. Actívelo en Herramientas > Complementos para editar estas propiedades.\tМодуль отключён · настройки сохранены. Для редактирования включите его в меню «Инструменты > Плагины».\tМодуль вимкнено · налаштування збережено. Для редагування увімкніть його в меню «Інструменти > Плагіни».\tModul deaktiviert · Einstellungen bleiben erhalten. Zum Bearbeiten unter Werkzeuge > Erweiterungen aktivieren.
GeneralSettings\tSettings — General\tAjustes — General\tНастройки — Общие\tНалаштування — Загальні\tEinstellungen — Allgemein
LocalizationCsv\tLocalization CSV\tCSV de traducciones\tCSV с переводами\tCSV із перекладами\tÜbersetzungs-CSV
ReloadLocalizationCsv\tReload localization CSV\tRecargar CSV de traducciones\tПерезагрузить CSV с переводами\tПерезавантажити CSV із перекладами\tÜbersetzungs-CSV neu laden
EmptyTranslationFallback\tEmpty translations fall back to English.\tLas traducciones vacías usan el texto en inglés.\tДля пустых переводов используется английский текст.\tДля порожніх перекладів використовується англійський текст.\tBei leeren Übersetzungen wird der englische Text verwendet.
CsvColumns\tCSV columns are key followed by locale codes.\tLas columnas del CSV son key y, después, los códigos de idioma.\tСтолбцы CSV: key, затем коды языков.\tСтовпці CSV: key, потім коди мов.\tDie CSV-Spalten beginnen mit key, gefolgt von den Sprachcodes.
LocalizationUndoScope\tLocalization changes are separate from scene Undo.\tLos cambios de idioma son independientes de Deshacer en las escenas.\tИзменение языка не входит в историю отмены сцен.\tЗміна мови не входить до історії скасування сцен.\tSprachänderungen sind von der Rückgängig-Funktion für Szenen getrennt.
AvailableLanguages\tAvailable languages\tIdiomas disponibles\tДоступные языки\tДоступні мови\tVerfügbare Sprachen
MissingTranslations\tMissing translations\tTraducciones pendientes\tОтсутствующие переводы\tВідсутні переклади\tFehlende Übersetzungen
EmbeddedTranslations\tExternal CSV unavailable; embedded translations are in use.\tEl CSV externo no está disponible; se usan las traducciones integradas.\tВнешний CSV недоступен; используются встроенные переводы.\tЗовнішній CSV недоступний; використовуються вбудовані переклади.\tExterne CSV nicht verfügbar; integrierte Übersetzungen werden verwendet.
PreparingLocalization\tPreparing localization…\tPreparando traducciones…\tПодготовка переводов…\tПідготовка перекладів…\tÜbersetzungen werden vorbereitet…
"""


def normalize(text):
    # Qt single ampersand is a mnemonic; doubled ampersand is a literal one.
    return text.replace("&&", "\0").replace("&", "").replace("\0", "&").strip()


def read_ini(path):
    values = {}
    for line_no, line in enumerate(path.read_text(encoding="utf-8-sig").splitlines(), 1):
        if not line.strip() or line.lstrip().startswith(("#", ";", "[")):
            continue
        match = ASSIGNMENT.match(line)
        if not match:
            raise ValueError(f"Unsupported OBS locale syntax {path}:{line_no}: {line}")
        if line[match.end():].strip():
            # OBS lookup_goto_nextline discards trailing tokens, even malformed
            # Crowdin lines with literal \\n between two intended assignments.
            INI_WARNINGS.append({"path": path.relative_to(ROOT).as_posix(), "line": line_no,
                                 "kind": "trailing tokens ignored, matching OBS text-lookup.c", "text": line[match.end():]})
        key, value = match.groups()
        # Literal order matches libobs/util/text-lookup.c convert_string.
        for escaped, decoded in ((r"\n", "\n"), (r"\t", "\t"), (r"\r", "\r"), (r'\"', '"')):
            value = value.replace(escaped, decoded)
        if key in values:
            INI_WARNINGS.append({"path": path.relative_to(ROOT).as_posix(), "line": line_no,
                                 "kind": "duplicate key; final assignment wins, matching OBS HASH_REPLACE_STR", "key": key})
        values[key] = (value, line_no)
    return values


def keyed_source13_seeds():
    path = ROOT / "localization/source13-keyed-seeds.json"
    result = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
    for key, values in result.items():
        if not key.startswith("SkyLive.CameraControl.") or set(values) != set(REQUIRED) or not all(values.values()):
            raise ValueError(f"Invalid contextual Source13 seed: {key}")
    return result


def app_seeds():
    result = {}
    for line in APP.splitlines():
        cells = line.split("\t")
        if len(cells) != 6:
            raise ValueError(f"App translation seed must have six columns: {cells}")
        result["SkyLive." + cells[0]] = dict(zip(REQUIRED, cells[1:]))
    result.update(keyed_source13_seeds())
    return result


def extended_app_seeds():
    path = ROOT / "localization/app-seeds.json"
    result = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
    tooltips = ROOT / "localization/tooltip-seeds.json"
    if tooltips.exists():
        for english, values in json.loads(tooltips.read_text(encoding="utf-8")).items():
            if set(values) != set(REQUIRED[1:]) or not all(values.values()):
                raise ValueError(f"Invalid four-language tooltip seed: {english}")
            result.setdefault(english, values)
    for filename in ("preview-snapping-seeds.json", "text-case-transform-seeds.json", "text-font-file-seeds.json", "text-system-fonts-seeds.json", "obs-text-outline-seeds.json", "obs-text-gradient-seeds.json", "text-file-source-seeds.json", "text-file-missing-files-seeds.json", "image-source-properties-seeds.json", "camera-vertical-flip-seeds.json", "craft-vignette-seeds.json", "craft-sepia-seeds.json", "craft-photo-filter-seeds.json", "craft-gaussian-blur-seeds.json", "craft-gaussian-scope-seeds.json", "exeldro-freeze-manual-seeds.json", "obs-render-delay-seeds.json", "scene-canvas-basis-seeds.json", "missing-files-seeds.json", "failed-open-resource-repair-seeds.json", "browser-missing-files-seeds.json", "obs-group-interchange-seeds.json", "obs-relative-coordinates-seeds.json", "filter-add-search-seeds.json", "fade-to-color-controls-seeds.json", "last-document-startup-seeds.json", "source-audio-hotkeys-seeds.json", "source-push-audio-seeds.json", "source-playback-hotkeys-seeds.json", "source-hotkeys-obs-seeds.json", "obs-media-source-interchange-seeds.json", "obs-media-hotkey-pairs-seeds.json", "hotkey-press-arbitration-seeds.json", "source-size-output-resize-seeds.json", "slideshow-directories-seeds.json", "projector-restoration-seeds.json", "scene-collection-library-seeds.json", "global-studio-shortcuts-seeds.json", "obs-noise-suppression-seeds.json", "craft-audio-effects-seeds.json", "slideshow-controls-seeds.json", "slideshow-randomization-seeds.json", "source-color-tags-seeds.json", "projector-settings-seeds.json", "audio-multitrack-seeds.json", "projectors-multiview-seeds.json", "obs-scale-filter-seeds.json", "obs-scale-geometry-seeds.json", "slideshow-transitions-seeds.json", "slideshow-canvas-visibility-seeds.json", "luma-wipe-seeds.json", "obs-compressor-sidechain-seeds.json", "camera-av-activation-seeds.json", "obs-scroll-filter-seeds.json", "obs-color-lut-seeds.json", "audio-post-dsp-history-seeds.json", "audio-sync-offset-seeds.json", "global-output-hotkeys-seeds.json", "group-source-seeds.json", "recording-settings-seeds.json",
                     "recording-runtime-seeds.json", "media-source-seeds.json",
                     "craft-audio-seeds.json", "advanced-audio-seeds.json", "hotkey-seeds.json", "color-correction-seeds.json",
                     "global-audio-devices-seeds.json", "recording-warning-seeds.json", "appearance-seeds.json",
                     "obs-audio-filter-seeds.json", "obs-keyer-seeds.json", "scene-ownership-seeds.json", "document-seeds.json", "streaming-settings-seeds.json", "color-properties-seeds.json", "streaming-runtime-seeds.json", "obs-file-ui-seeds.json", "settings-workflow-seeds.json", "media-transport-seeds.json", "contextual-status-seeds.json", "dock-columns-seeds.json", "obs-dynamics-seeds.json", "obs-dynamics-scope-seeds.json", "window-wgc-seeds.json", "obs32-layout-seeds.json", "plugin-entry-point-seeds.json", "camera-selection-seeds.json", "camera-craft-filter-seeds.json", "person-background-ui-seeds.json", "person-inference-cadence-seeds.json", "setup-seeds.json", "scene-notes-dock-seeds.json", "edit-clipboard-ui-seeds.json", "mixer-view-seeds.json", "craft-camera-six-seeds.json", "source-toolbar-seeds.json", "native-browse-seeds.json", "virtual-camera-ui-seeds.json", "source13-ui-seeds.json", "source13-plugin-scope-seeds.json", "ndi-ui-seeds.json", "ndi-plugin-scope-seeds.json", "statistics-seeds.json", "obs-luma-sharpen-seeds.json", "obs-crop-pad-seeds.json", "source13-supplement-seeds.json", "item-compositing-seeds.json", "accessibility-seeds.json", "application-audio-seeds.json", "replay-buffer-seeds.json", "multi-camera-seeds.json", "source-scale-filtering-seeds.json", "recording-pause-seeds.json", "virtual-camera-routing-seeds.json", "browser-source-seeds.json", "output-resolution-seeds.json", "display-capture-methods-seeds.json", "rtmps-seeds.json", "obs-noise-gate-seeds.json", "obs-upward-compressor-seeds.json", "browser-audio-seeds.json", "game-capture-seeds.json", "ndi-output-seeds.json", "nvenc-h264-seeds.json", "obs-three-band-eq-seeds.json", "stream-service-presets-seeds.json", "obs-image-mask-seeds.json", "output-shortcuts-seeds.json", "browser-interaction-seeds.json", "screenshot-seeds.json", "windows-media-decoder-seeds.json", "matroska-recording-seeds.json", "camera-audio-source-seeds.json", "setup-default-audio-seeds.json", "filter-defaults-seeds.json", "advanced-process-seeds.json", "recording-remux-seeds.json", "stream-reconnect-seeds.json", "output-audio-tracks-seeds.json"):
        path = ROOT / "localization" / filename
        if path.exists():
            for english, values in json.loads(path.read_text(encoding="utf-8")).items():
                if set(values) != set(REQUIRED[1:]) or not all(values.values()):
                    raise ValueError(f"Invalid four-language seed in {filename}: {english}")
                result.setdefault(english, values)
    return result


def app_invariants():
    path = ROOT / "localization/app-invariants.json"
    result = json.loads(path.read_text(encoding="utf-8")) if path.exists() else {}
    recording = ROOT / "localization/recording-invariants.json"
    if recording.exists():
        for text, reason in json.loads(recording.read_text(encoding="utf-8")).items():
            result.setdefault(text, reason)
    return result


def reviewed_source_seeds():
    path = ROOT / "localization/required-source-seeds.json"
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else {"values": {}, "language_independent_english": []}


def complete_reviewed_source_cells(rows):
    seeds = reviewed_source_seeds()["values"]
    for values in rows.values():
        for locale, translation in seeds.get(values.get("en-US", ""), {}).items():
            if not values.get(locale):
                values[locale] = translation


def complete_app_cells(rows, seeds):
    """Explicit one-time filling of blank app cells; no source/user edits reset."""
    changed = 0
    contextual = keyed_source13_seeds()
    for key, values in rows.items():
        if not key.startswith("SkyLive.") or key == "SkyLive.LanguageName" or key.startswith("SkyLive.ThemeBrand."):
            continue
        translations = contextual.get(key, seeds.get(values.get("en-US", ""), {}))
        for language in REQUIRED[1:]:
            if not values.get(language, "") and translations.get(language, ""):
                values[language] = translations[language]
                changed += 1
    return changed


def rust_string_tokens(source):
    """Yield actual ordinary/raw strings, skipping comments and character literals."""
    i = 0
    while i < len(source):
        if source.startswith("//", i):
            end = source.find("\n", i)
            i = len(source) if end < 0 else end
        elif source.startswith("/*", i):
            i, depth = i + 2, 1
            while i < len(source) and depth:
                if source.startswith("/*", i):
                    depth, i = depth + 1, i + 2
                elif source.startswith("*/", i):
                    depth, i = depth - 1, i + 2
                else:
                    i += 1
        elif source[i] == 'r' and (raw := re.match(r'r(#{0,32})"', source[i:i + 34])):
            close = '"' + raw.group(1)
            start = i + raw.end()
            end = source.find(close, start)
            if end < 0:
                return
            yield i, source[start:end]
            i = end + len(close)
        elif source[i] == '"':
            token = RUST_STRING.match(source, i)
            if not token:
                return
            try:
                value = json.loads(token[0])
            except json.JSONDecodeError:
                value = None
            if value is not None:
                yield i, value
            i = token.end()
        elif source[i] == "'" and (char := re.match(r"'(?:\\.|[^'\\\n])'", source[i:])):
            i += char.end()
        else:
            i += 1


def rust_file_dependencies(file, source):
    code = rust_lex.mask(source)
    found = set()
    declaration = re.compile(r'(?P<attrs>(?:#\s*\[[^\]]+\]\s*)*)(?:pub(?:\s*\([^)]*\))?\s+)?(?P<mod>mod)\s+(?P<name>\w+)\s*;')
    for match in declaration.finditer(source):
        if code[match.start("mod"):match.end("mod")] != "mod":
            continue
        path = re.search(r'#\s*\[\s*path\s*=\s*("(?:\\.|[^"\\])*")\s*\]', match.group("attrs"))
        if path:
            candidates = [file.parent / json.loads(path.group(1))]
        else:
            base = file.parent if file.name in {"main.rs", "lib.rs", "mod.rs"} else file.parent / file.stem
            candidates = [base / (match.group("name") + ".rs"), base / match.group("name") / "mod.rs"]
        found.update(candidate.resolve() for candidate in candidates if candidate.is_file())
    for match in re.finditer(r'\binclude\s*!\s*\(\s*("(?:\\.|[^"\\])*")\s*\)', source):
        if code[match.start():match.start() + 7] == "include":
            candidate = file.parent / json.loads(match.group(1))
            if candidate.is_file():
                found.add(candidate.resolve())
    return found


def external_test_modules(files):
    """Follow test module/include edges, retaining anything also used in production."""
    all_edges, production_edges, test_roots = {}, {}, set()
    for file in files:
        source = file.read_text(encoding="utf-8")
        path = file.resolve()
        all_edges[path] = rust_file_dependencies(file, source)
        production_edges[path] = rust_file_dependencies(file, production_rust(source))
        test_roots.update(all_edges[path] - production_edges[path])

    def closure(roots, edges):
        reached, pending = set(), list(roots)
        while pending:
            path = pending.pop()
            if path not in reached:
                reached.add(path)
                pending.extend(edges.get(path, ()))
        return reached

    candidates = closure(test_roots, all_edges)
    production = closure(set(all_edges) - candidates, production_edges)
    return candidates - production


def production_rust(source):
    """Blank cfg(test) items/branches while preserving source line coordinates."""
    code = rust_lex.mask(source)
    result = list(source)
    for match in re.finditer(r'#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]', code):
        brace, semicolon = code.find("{", match.end()), code.find(";", match.end())
        if semicolon >= 0 and (brace < 0 or semicolon < brace):
            end = semicolon + 1
        elif brace >= 0:
            end = rust_lex.close_call(code, brace, "{", "}")
        else:
            continue
        for index in range(match.start(), end):
            if result[index] != "\n":
                result[index] = " "
    return "".join(result)


def build_catalog(include_reviewed_source=True):
    INI_WARNINGS.clear()
    frontend = OBS / "frontend/data/locale"
    languages = REQUIRED + sorted({path.stem for path in frontend.glob("*.ini")} - set(REQUIRED))
    rows, provenance = {}, {}
    def import_directory(directory, prefix):
        for locale in languages:
            file = directory / (locale + ".ini")
            if not file.exists():
                continue
            for original, (value, line) in read_ini(file).items():
                key = prefix + original
                row = rows.setdefault(key, {})
                if locale in row and row[locale] != value:
                    raise ValueError(f"Conflicting module locale {key}/{locale}: {file}:{line}")
                row[locale] = value
                if locale == "en-US":
                    provenance.setdefault(key, []).append({"path": file.relative_to(ROOT).as_posix(), "line": line, "source_key": original})
    import_directory(frontend, "")
    modules = []
    for plugin in sorted((OBS / "plugins").iterdir()):
        if not plugin.is_dir():
            continue
        for file in sorted(plugin.rglob("en-US.ini")):
            if file.parent.name == "locale":
                import_directory(file.parent, f"Module.{plugin.name}.")
                modules.append({"module": plugin.name, "path": file.parent.relative_to(ROOT).as_posix()})
    for name in ("obs-browser", "obs-websocket", "obs-vst"):
        if any(module["module"] == name for module in modules):
            continue
        directory = ROOT / f"doc/plugins/obsproject--{name}/data/locale"
        if (directory / "en-US.ini").exists():
            import_directory(directory, f"Module.{name}.")
            modules.append({"module": name, "path": directory.relative_to(ROOT).as_posix()})
    native_names = {}
    section = None
    for line in (OBS / "frontend/data/locale.ini").read_text(encoding="utf-8-sig").splitlines():
        if line.startswith("["):
            section = line.strip("[]")
        elif line.startswith("Name=") and section:
            native_names[section] = line[5:].strip().strip('"')
    rows["SkyLive.LanguageName"] = {locale: native_names.get(locale) or rows.get("Language", {}).get(locale, "") for locale in languages}
    seeds = app_seeds()
    rows.update(seeds)
    provenance["SkyLive.LanguageName"] = [{"path": "doc/obs-studio/frontend/data/locale.ini", "kind": "native Names; locale file Language value used when absent"}]
    for key in seeds:
        provenance[key] = [{"path": "tools/localization_catalog.py", "kind": "assistant-authored application common labels; no native-linguist review"}]
    for key in keyed_source13_seeds():
        provenance[key] = [{"path": "localization/source13-keyed-seeds.json", "kind": "assistant-authored contextual camera label; explicit key only, no native-linguist review"}]
    for description_file in ("plugin-descriptions.json", "craft-audio-description-seeds.json", "obs-dynamics-description-seeds.json", "person-background-description-seeds.json"):
        plugin_descriptions = ROOT / "localization" / description_file
        if not plugin_descriptions.exists():
            continue
        descriptions = json.loads(plugin_descriptions.read_text(encoding="utf-8"))
        for key, values in descriptions.items():
            if not re.fullmatch(r"Plugin\..+\.Description(?:\.[A-Za-z0-9]+)?", key) or not all(values.get(locale) for locale in REQUIRED):
                raise ValueError(f"Invalid plugin description seed: {key}")
            rows[key] = dict(values)
            provenance[key] = [{"path": "localization/" + description_file, "kind": "assistant-authored five-language descriptions; original Russian preserved; no native-linguist review"}]
    plugin_catalog = ROOT / "doc/migration/plugin-catalog.json"
    if plugin_catalog.exists():
        for provider in json.loads(plugin_catalog.read_text(encoding="utf-8"))["catalog"]:
            english = provider["port_scope"]
            key = "SkyLive.PluginScope." + hashlib.sha256(english.encode()).hexdigest()[:12]
            rows.setdefault(key, {})["en-US"] = english
            provenance[key] = [{"path": "doc/migration/plugin-catalog.json", "kind": "public compiled provider scope"}]
    # Official theme names are brands. Publish editable English keys without
    # inventing translations or English-filling other locale columns.
    for name in ("Acri", "Classic", "Rachni"):
        key = "SkyLive.ThemeBrand." + name
        rows.setdefault(key, {})["en-US"] = name
        provenance[key] = [{"path": "localization/app-invariants.json", "kind": "official theme brand; English fallback, not translated coverage"}]
    # Source-derived aliases cover unmatched presentation literals. IDs and file
    # paths are excluded; translated sentence templates retain their placeholders.
    known = {normalize(values.get("en-US", "")) for key, values in rows.items() if not key.startswith("SkyLive.CameraControl.")}
    files = sorted((ROOT / "apps/desktop/src").rglob("*.rs"))
    test_only_files = external_test_modules(files)
    for file in files:
        if file.resolve() in test_only_files:
            continue
        text = production_rust(file.read_text(encoding="utf-8"))
        for start, value in rust_string_tokens(text):
            normalized = normalize(value)
            if not re.search(r"[A-Za-z]", value) or len(value) > 32768 or normalized in known:
                continue
            before = text[max(0, start - 100):start]
            # A nearby UiBuilder::new is not evidence that a panic contract or
            # widget identity is displayed. Exclude only the immediate argument;
            # actual Button::new/TextEdit hints and visible errors still enter.
            if re.search(r'(?:\.\s*expect|\b(?:id_salt|push_id|Id::new|Grid::new))\s*\(\s*\(?\s*$', before):
                continue
            ui_context = re.search(r'(?:label|heading|button|checkbox|radio|new|on_[a-z_]*hover_text|message)\s*\([^\n;]*$', before)
            if not (ui_context or (" " in value and not value.startswith(("http", "/", "doc/")))):
                continue
            slug = re.sub(r"[^A-Za-z0-9]+", "_", normalized).strip("_")[:44] or "Text"
            key = "SkyLive.Text." + slug + "." + hashlib.sha256(normalized.encode()).hexdigest()[:8]
            rows.setdefault(key, {})["en-US"] = value
            provenance.setdefault(key, []).append({"path": file.relative_to(ROOT).as_posix(), "line": text.count("\n", 0, start) + 1, "kind": "source-derived UI literal; cell translation provenance recorded separately"})
            known.add(normalized)
    # Camera capability/mode notes are emitted dynamically by the host and
    # passed to tr(ui, note); they are not desktop Rust string literals. This
    # explicit reviewed mapping publishes them without scanning backend logs.
    for filename in ("preview-snapping-seeds.json", "text-case-transform-seeds.json", "text-font-file-seeds.json", "text-system-fonts-seeds.json", "obs-text-outline-seeds.json", "obs-text-gradient-seeds.json", "text-file-source-seeds.json", "text-file-missing-files-seeds.json", "image-source-properties-seeds.json", "camera-vertical-flip-seeds.json", "craft-vignette-seeds.json", "craft-sepia-seeds.json", "craft-photo-filter-seeds.json", "craft-gaussian-blur-seeds.json", "craft-gaussian-scope-seeds.json", "exeldro-freeze-manual-seeds.json", "obs-render-delay-seeds.json", "scene-canvas-basis-seeds.json", "missing-files-seeds.json", "failed-open-resource-repair-seeds.json", "browser-missing-files-seeds.json", "obs-group-interchange-seeds.json", "obs-relative-coordinates-seeds.json", "filter-add-search-seeds.json", "fade-to-color-controls-seeds.json", "last-document-startup-seeds.json", "source-audio-hotkeys-seeds.json", "source-push-audio-seeds.json", "source-playback-hotkeys-seeds.json", "source-hotkeys-obs-seeds.json", "obs-media-source-interchange-seeds.json", "obs-media-hotkey-pairs-seeds.json", "hotkey-press-arbitration-seeds.json", "source-size-output-resize-seeds.json", "slideshow-directories-seeds.json", "projector-restoration-seeds.json", "scene-collection-library-seeds.json", "global-studio-shortcuts-seeds.json", "obs-noise-suppression-seeds.json", "craft-audio-effects-seeds.json", "slideshow-controls-seeds.json", "slideshow-randomization-seeds.json", "source-color-tags-seeds.json", "projector-settings-seeds.json", "audio-multitrack-seeds.json", "projectors-multiview-seeds.json", "obs-scale-filter-seeds.json", "obs-scale-geometry-seeds.json", "slideshow-transitions-seeds.json", "slideshow-canvas-visibility-seeds.json", "luma-wipe-seeds.json", "obs-compressor-sidechain-seeds.json", "camera-av-activation-seeds.json", "obs-scroll-filter-seeds.json", "obs-color-lut-seeds.json", "audio-post-dsp-history-seeds.json", "audio-sync-offset-seeds.json", "global-output-hotkeys-seeds.json", "camera-seeds.json", "recording-settings-seeds.json",
                     "recording-runtime-seeds.json", "group-source-seeds.json",
                     "media-source-seeds.json", "craft-audio-seeds.json",
                     "advanced-audio-seeds.json", "hotkey-seeds.json", "color-correction-seeds.json",
                     "global-audio-devices-seeds.json", "recording-warning-seeds.json", "appearance-seeds.json",
                     "obs-audio-filter-seeds.json", "obs-keyer-seeds.json", "scene-ownership-seeds.json", "document-seeds.json", "streaming-settings-seeds.json", "color-properties-seeds.json", "streaming-runtime-seeds.json", "obs-file-ui-seeds.json", "settings-workflow-seeds.json", "media-transport-seeds.json", "contextual-status-seeds.json", "dock-columns-seeds.json", "obs-dynamics-seeds.json", "obs-dynamics-scope-seeds.json", "window-wgc-seeds.json", "obs32-layout-seeds.json", "plugin-entry-point-seeds.json", "camera-selection-seeds.json", "camera-craft-filter-seeds.json", "person-background-ui-seeds.json", "person-inference-cadence-seeds.json", "setup-seeds.json", "scene-notes-dock-seeds.json", "edit-clipboard-ui-seeds.json", "mixer-view-seeds.json", "craft-camera-six-seeds.json", "source-toolbar-seeds.json", "native-browse-seeds.json", "virtual-camera-ui-seeds.json", "source13-ui-seeds.json", "source13-plugin-scope-seeds.json", "ndi-ui-seeds.json", "ndi-plugin-scope-seeds.json", "statistics-seeds.json", "obs-luma-sharpen-seeds.json", "obs-crop-pad-seeds.json", "source13-supplement-seeds.json", "item-compositing-seeds.json", "accessibility-seeds.json", "application-audio-seeds.json", "replay-buffer-seeds.json", "multi-camera-seeds.json", "source-scale-filtering-seeds.json", "recording-pause-seeds.json", "virtual-camera-routing-seeds.json", "browser-source-seeds.json", "output-resolution-seeds.json", "display-capture-methods-seeds.json", "rtmps-seeds.json", "obs-noise-gate-seeds.json", "obs-upward-compressor-seeds.json", "browser-audio-seeds.json", "game-capture-seeds.json", "ndi-output-seeds.json", "nvenc-h264-seeds.json", "obs-three-band-eq-seeds.json", "stream-service-presets-seeds.json", "obs-image-mask-seeds.json", "output-shortcuts-seeds.json", "browser-interaction-seeds.json", "screenshot-seeds.json", "windows-media-decoder-seeds.json", "matroska-recording-seeds.json", "camera-audio-source-seeds.json", "setup-default-audio-seeds.json", "filter-defaults-seeds.json", "advanced-process-seeds.json", "recording-remux-seeds.json", "stream-reconnect-seeds.json", "output-audio-tracks-seeds.json"):
        runtime_source = ROOT / "localization" / filename
        if not runtime_source.exists():
            continue
        for english, translations in json.loads(runtime_source.read_text(encoding="utf-8")).items():
            if set(translations) != set(REQUIRED[1:]) or not all(translations.values()):
                raise ValueError(f"Invalid runtime translation seed in {filename}: {english}")
            normalized = normalize(english)
            if normalized in known:
                continue
            slug = re.sub(r"[^A-Za-z0-9]+", "_", normalized).strip("_")[:44] or "Text"
            key = "SkyLive.Text." + slug + "." + hashlib.sha256(normalized.encode()).hexdigest()[:8]
            rows[key] = {"en-US": english, **translations}
            provenance[key] = [{"path": "localization/" + filename, "kind": "assistant-reviewed dynamic application label/help/validation; no native-linguist review"}]
            known.add(normalized)
    complete_app_cells(rows, extended_app_seeds())
    if include_reviewed_source:
        complete_reviewed_source_cells(rows)
    return languages, rows, provenance, modules


def read_csv(path):
    with path.open(encoding="utf-8-sig", newline="") as file:
        reader = csv.reader(file, strict=True)
        header = next(reader, None)
        if not header or header[0] != "key" or len(set(header)) != len(header):
            raise ValueError("CSV needs unique key/language headers")
        if len(header) > 129 or "en-US" not in header:
            raise ValueError("CSV requires en-US and at most 128 locale columns")
        rows = {}
        for number, values in enumerate(reader, 2):
            if len(values) != len(header) or not values[0] or values[0] in rows:
                raise ValueError(f"Duplicate key/invalid CSV columns at logical row {number}")
            if any(len(value.encode("utf-8")) > 32768 for value in values):
                raise ValueError(f"CSV cell exceeds 32768 UTF-8 bytes at logical row {number}")
            if len(rows) >= 32768:
                raise ValueError("CSV key count exceeds 32768")
            rows[values[0]] = dict(zip(header[1:], values[1:]))
        return header[1:], rows


def merge_rows(languages, generated, existing_languages, existing):
    languages = list(languages) + [language for language in existing_languages if language not in languages]
    merged = {key: dict(values) for key, values in generated.items()}
    for key, values in existing.items():
        # Every existing cell, even empty, belongs to the editor. New source keys
        # and newly added columns can be seeded without erasing manual work.
        merged.setdefault(key, {}).update(values)
    return languages, merged


def coverage(languages, rows, generated):
    namespaces = {"frontend": lambda key: not key.startswith(("SkyLive.", "Module.", "Plugin.")),
                  "modules": lambda key: key.startswith("Module."), "application": lambda key: key.startswith("SkyLive."),
                  "plugin_metadata": lambda key: key.startswith("Plugin.")}
    by_namespace = {}
    for name, predicate in namespaces.items():
        selected = [values for key, values in rows.items() if predicate(key)]
        by_namespace[name] = {"keys": len(selected), "missing_required": {language: sum(not values.get(language, "") for values in selected) for language in REQUIRED}}
    invariants = app_invariants()
    technical = {key: invariants[values.get("en-US", "")] for key, values in rows.items() if key.startswith("SkyLive.") and values.get("en-US", "") in invariants}
    user_facing = {key: values for key, values in rows.items() if key.startswith("SkyLive.") and key not in technical}
    return {"keys": len(rows), "languages": len(languages), "missing_source_keys": sorted(set(generated) - set(rows)),
            "missing_by_locale": {language: sum(not values.get(language, "") for values in rows.values()) for language in languages},
            "app_keys": sum(key.startswith("SkyLive.") for key in rows),
            "curated_app_keys": len(app_seeds()),
            "coverage_by_namespace": by_namespace,
            "app_assistant_seed_english_count": len(extended_app_seeds()),
            "authored_seed_quality": "Assistant-authored/reference-checked; not human/native-linguist reviewed",
            "app_invariant_rows": technical,
            "app_user_facing_keys": len(user_facing),
            "app_user_facing_missing": {language: [key for key, values in user_facing.items() if not values.get(language, "")] for language in REQUIRED},
            "english_source_differences": [key for key in sorted(set(rows) & set(generated)) if rows[key].get("en-US", "") != generated[key].get("en-US", "")],
            "frontend_keys": sum(not key.startswith(("SkyLive.", "Module.", "Plugin.")) for key in rows),
            "plugin_metadata_keys": sum(key.startswith("Plugin.") for key in rows),
            "module_keys": sum(key.startswith("Module.") for key in rows)}


def serialize_csv(languages, rows):
    stream = io.StringIO(newline="")
    writer = csv.writer(stream, lineterminator="\r\n", quoting=csv.QUOTE_ALL)
    writer.writerow(["key", *languages])
    for key in sorted(rows):
        writer.writerow([key, *[rows[key].get(language, "") for language in languages]])
    return b"\xef\xbb\xbf" + stream.getvalue().encode("utf-8")


class Tests(unittest.TestCase):
    def test_non_ui_immediate_arguments_are_not_visible_literals(self):
        pattern = r'(?:\.\s*expect|\b(?:id_salt|push_id|Id::new|Grid::new))\s*\(\s*\(?\s*$'
        for before in ['UiBuilder::new().id_salt((', 'foo.expect(', 'egui::Id::new(', 'ui.push_id(']:
            self.assertRegex(before, pattern)
        for before in ['ui.button(', 'Button::new(', 'ui.label(', 'i18n::tr(ui, ']:
            self.assertNotRegex(before, pattern)
    def test_csv_quotes_newlines_bom(self):
        data = serialize_csv(["en-US", "ru-RU"], {"SkyLive.Test": {"en-US": 'a,"b"\nnext', "ru-RU": "Привет"}})
        self.assertTrue(data.startswith(b"\xef\xbb\xbf"))
        rows = list(csv.reader(io.StringIO(data.decode("utf-8-sig"), newline=""), strict=True))
        self.assertEqual(rows[1], ["SkyLive.Test", 'a,"b"\nnext', "Привет"])
    def test_merge_preserves_empty_and_user_cells(self):
        languages, rows = merge_rows(["en-US", "ru-RU"], {"A": {"en-US": "New", "ru-RU": "Новый"}, "B": {"en-US": "Added"}}, ["en-US", "ru-RU"], {"A": {"en-US": "Edited", "ru-RU": ""}, "Custom": {"en-US": "Own"}})
        self.assertEqual(rows["A"], {"en-US": "Edited", "ru-RU": ""})
        self.assertEqual(rows["B"]["en-US"], "Added")
        self.assertIn("Custom", rows)
    def test_real_seeds_and_placeholder_preservation(self):
        for key, values in app_seeds().items():
            self.assertEqual(set(values), set(REQUIRED))
            for value in values.values():
                self.assertTrue(value)
                self.assertEqual(re.findall(r"\{[^}]+\}", value), re.findall(r"\{[^}]+\}", values["en-US"]))
    def test_mnemonics(self):
        self.assertEqual(normalize("&File"), "File")
        self.assertEqual(normalize("Research && Development"), "Research & Development")
    def test_reader_rejects_duplicate_rows_headers_and_malformed_width(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "test.csv"
            for text in ['key,en-US,en-US\nA,a,b\n', 'key,en-US\nA,a\nA,b\n', 'key,en-US\nA,a,b\n', 'key,en-US\nA,"unterminated\n']:
                path.write_text(text, encoding="utf-8-sig")
                with self.assertRaises((ValueError, csv.Error)):
                    read_csv(path)
    def test_english_and_utf8_cell_budget(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "test.csv"
            path.write_bytes(serialize_csv(["ru-RU"], {"A": {"ru-RU": "a"}}))
            with self.assertRaises(ValueError):
                read_csv(path)
            path.write_bytes(serialize_csv(["en-US"], {"A": {"en-US": "я" * 16385}}))
            with self.assertRaises(ValueError):
                read_csv(path)

    def test_extended_translations_preserve_rust_placeholder_tokens(self):
        placeholders = lambda value: re.findall(r"\{[^{}]*\}", value)
        for english, translations in extended_app_seeds().items():
            self.assertEqual(set(translations), set(REQUIRED[1:]), english)
            for language, translation in translations.items():
                self.assertTrue(translation, (english, language))
                self.assertEqual(placeholders(english), placeholders(translation), (english, language))
        self.assertFalse(set(extended_app_seeds()) & set(app_invariants()))

    def test_explicit_app_completion_preserves_existing_cells_and_source_translations(self):
        rows = {"SkyLive.A": {"en-US": "New", "ru-RU": "Изменено", "de-DE": ""},
                "Basic.Source": {"en-US": "New", "de-DE": ""}}
        complete_app_cells(rows, {"New": {"ru-RU": "Новый", "de-DE": "Neu"}})
        self.assertEqual(rows["SkyLive.A"]["ru-RU"], "Изменено")
        self.assertEqual(rows["SkyLive.A"]["de-DE"], "Neu")
        self.assertEqual(rows["Basic.Source"]["de-DE"], "")

    def test_only_proven_test_modules_and_branches_are_excluded(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            main = root / "main.rs"
            main.write_text('#[cfg(test)]\n#[path = "layout.rs"]\nmod layout;\nmod shared_tests;\n', encoding="utf-8")
            layout = root / "layout.rs"
            layout.write_text('ui.label("test fixture");', encoding="utf-8")
            shared = root / "shared_tests.rs"
            shared.write_text('ui.label("production despite filename");', encoding="utf-8")
            self.assertEqual(external_test_modules([main, layout, shared]), {layout.resolve()})
        source = 'ui.label("Before"); #[cfg(test)] { ui.label("headless fixture"); } #[cfg(not(test))] { ui.label("Native"); } #[cfg(test)] mod tests { fn fixture() { ui.label("test only"); } } ui.label("After");'
        production = production_rust(source)
        self.assertIn('ui.label("Before")', production)
        self.assertIn('ui.label("Native")', production)
        self.assertIn('ui.label("After")', production)
        self.assertNotIn('headless fixture', production)
        self.assertNotIn('test only', production)

    def test_dynamic_camera_notes_have_explicit_translated_catalog_keys(self):
        camera = json.loads((ROOT / "localization/camera-seeds.json").read_text(encoding="utf-8"))
        _, rows, provenance, _ = build_catalog()
        dynamic = {key: values for key, values in rows.items()
                   if any(item.get("path") == "localization/camera-seeds.json" for item in provenance.get(key, []))}
        self.assertTrue(dynamic, "dynamic host notes must not rely on desktop literal extraction")
        for values in dynamic.values():
            self.assertIn(values["en-US"], camera)
            self.assertTrue(all(values.get(locale) for locale in REQUIRED))
            for locale in REQUIRED[1:]:
                self.assertEqual(values[locale], camera[values["en-US"]][locale])

    def test_test_only_includes_are_transitive_but_shared_production_files_survive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sources = {
                "main.rs": '#[cfg(test)] mod checks { include!("fixture.rs"); } include!("shared.rs"); // include!("comment.rs");\n',
                "fixture.rs": 'include!("nested.rs"); include!("shared.rs");',
                "nested.rs": 'ui.label("only test text");',
                "shared.rs": 'ui.label("real public label");',
                "comment.rs": 'ui.label("unrelated file");',
            }
            for name, source in sources.items():
                (root / name).write_text(source, encoding="utf-8")
            self.assertEqual(external_test_modules(list(root.glob("*.rs"))),
                             {root / "fixture.rs", root / "nested.rs"})

    def test_rust_string_tokens_skip_nested_comments_and_keep_raw_boundaries(self):
        source = '''// ui.label("not visible");
/* outer "comment" /* nested "comment" */ */
ui.label(r##"raw "quoted" public label"##); if ok { ui.label("First"); } else { ui.label("Second"); }
let quote = '"'; let empty = ""; ui.label("escaped \\"label\\"");'''
        self.assertEqual([value for _, value in rust_string_tokens(source)],
                         ['raw "quoted" public label', "First", "Second", "", 'escaped "label"'])
        for start, _ in rust_string_tokens(source):
            self.assertIn(source[start], {'r', '"'})

    def test_plugin_description_ids_russian_and_required_columns_match_source_catalog(self):
        descriptions = json.loads((ROOT / "localization/plugin-descriptions.json").read_text(encoding="utf-8"))
        providers = json.loads((ROOT / "doc/migration/plugin-catalog.json").read_text(encoding="utf-8"))["catalog"]
        # Keep the original 127 third-party + 16 built-in providers intact;
        # bundled application providers extend rather than replace that catalog.
        self.assertEqual(sum(provider["origin"] != "Bundled" for provider in providers), 143)
        self.assertEqual({provider["id"] for provider in providers if provider["origin"] == "Bundled"},
                         {"skylive/craft-audio-effects", "skylive/craft-video-effects"})
        historical = dict(descriptions)
        descriptions.update(json.loads((ROOT / "localization/craft-audio-description-seeds.json").read_text(encoding="utf-8")))
        descriptions.update(json.loads((ROOT / "localization/obs-dynamics-description-seeds.json").read_text(encoding="utf-8")))
        descriptions.update(json.loads((ROOT / "localization/person-background-description-seeds.json").read_text(encoding="utf-8")))
        self.assertEqual(set(historical), {"Plugin." + provider["id"] + ".Description" for provider in providers})
        self.assertTrue({provider["description_key"] for provider in providers} <= set(descriptions))
        for provider in providers:
            values = descriptions[provider["description_key"]]
            source_locale = "en-US" if provider["origin"] == "Bundled" else "ru-RU"
            # The supplied Russian reference description remains immutable;
            # a versioned compiled-scope description is authored separately.
            if provider["id"] in {"obsproject/obs-studio:obs-filters", "royshil/obs-backgroundremoval"}:
                original = historical["Plugin." + provider["id"] + ".Description"]
                self.assertEqual(original[source_locale], provider["description"])
                self.assertEqual(provider["description_key"], "Plugin." + provider["id"] + ".Description." + ("Dynamics1" if provider["id"]=="obsproject/obs-studio:obs-filters" else "Selfie1"))
            else:
                self.assertEqual(values[source_locale], provider["description"])
            self.assertTrue(all(values[locale] for locale in REQUIRED))
        key = next(iter(descriptions))
        edited = {key: {"en-US": "Edited", "de-DE": "Eigene Bearbeitung", "ru-RU": ""}}
        _, merged = merge_rows(REQUIRED, descriptions, REQUIRED, edited)
        self.assertEqual(merged[key]["en-US"], "Edited")
        self.assertEqual(merged[key]["de-DE"], "Eigene Bearbeitung")
        self.assertEqual(merged[key]["ru-RU"], "")

    def test_reviewed_source_values_preserve_placeholder_contract(self):
        contract = lambda text: Counter(re.findall(r"\{\{|\}\}|\{[^{}]*\}|%L?\d+|%n", text))
        for english, values in reviewed_source_seeds()["values"].items():
            self.assertTrue(set(values) <= set(REQUIRED[1:]))
            for translated in values.values():
                self.assertTrue(translated)
                self.assertEqual(contract(english), contract(translated), english)
        rows = {"Basic.Name": {"en-US": "Name", "de-DE": "User edit"}}
        complete_reviewed_source_cells(rows)
        self.assertEqual(rows["Basic.Name"]["de-DE"], "User edit")


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
        sys.stderr.reconfigure(encoding="utf-8")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Read/report coverage only; never overwrite")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--complete-app", action="store_true", help="Explicitly fill still-empty SkyLive cells from reviewed four-language app dictionary; preserve every nonempty cell")
    args = parser.parse_args()
    if args.self_test:
        result = unittest.main(argv=[sys.argv[0]], exit=False)
        raise SystemExit(0 if result.result.wasSuccessful() else 1)
    languages, generated, provenance, modules = build_catalog()
    existing_languages, existing = read_csv(CSV) if CSV.exists() else ([], {})
    if args.check:
        report = coverage(existing_languages, existing, generated)
        print(json.dumps(report, ensure_ascii=False, indent=2))
        if not CSV.exists() or report["missing_source_keys"] or any(language not in existing_languages for language in languages):
            raise SystemExit(1)
        return
    languages, rows = merge_rows(languages, generated, existing_languages, existing)
    completed = complete_app_cells(rows, extended_app_seeds()) if args.complete_app else 0
    data = serialize_csv(languages, rows)
    if len(data) > 32 * 1024 * 1024 or len(rows) > 32768 or len(languages) > 128:
        raise ValueError("CSV exceeds the host localization admission profile")
    CSV.parent.mkdir(exist_ok=True)
    temporary = CSV.with_suffix(".csv.tmp")
    temporary.write_bytes(data)
    temporary.replace(CSV)
    report = {**coverage(languages, rows, generated), "csv_bytes": len(data),
              "csv_sha256": hashlib.sha256(data).hexdigest(), "modules": modules,
              "source_reference": "doc/obs-studio @72e86655715d9a25d69ce446aa4dd44ba58e10d2; fallback built-in repository commits in doc/plugins/manifest.json",
              "cell_policy": ("Explicit app completion: existing nonempty cells and source translations preserved; only empty SkyLive cells filled from reviewed dictionary" if args.complete_app else "Existing cells (including blanks) preserved; empty translations deliberately fall back to English; no English fill into untranslated locales"),
              "source_parse_warnings": INI_WARNINGS,
              "app_cells_filled_explicitly": completed,
              "key_provenance": provenance}
    (CSV.parent / "catalog-report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: value for key, value in report.items() if key not in {"key_provenance", "modules", "source_parse_warnings"}}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
