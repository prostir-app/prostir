# Тексти Prostir українською: один файл для програми на ПК, вікна налаштувань і
# програми в шоломі. Чого тут бракує, береться з en.ftl.

language-name = Українська

### PC app

## Тексти з числом: слово стоїть у формі, якої потребує число

pc-limit-monitors = Щонайбільше { $n } { $n ->
    [one] монітор
    [few] монітори
   *[many] моніторів
}
pc-limit-windows = Щонайбільше { $n } { $n ->
    [one] вікно
    [few] вікна
   *[many] вікон
}
pc-limit-physical = Щонайбільше { $n } { $n ->
    [one] екран ПК
    [few] екрани ПК
   *[many] екранів ПК
}
pc-quality-custom = Власна: { $n } Мбіт/с
pc-limit-streams = Щонайбільше { $n } { $n ->
    [one] екран
    [few] екрани
   *[many] екранів
} разом

## src/ctl.rs

pc-ctl-waiting-for-the-headset-asleep = Чекає на шолом (спить)
pc-ctl-not-running-port = Не працює: порт
pc-ctl-is-taken-by-another-program = зайнятий іншою програмою
pc-ctl-not-running = Не працює
pc-ctl-no-session-bus = Немає сеансової шини
pc-ctl-prostir-is-not-running = Prostir не працює
pc-ctl-no-answer-in-time = Немає відповіді вчасно
pc-ctl-whole-desktop = { " (уся стільниця)" }
pc-ctl-saved = Збережено:
pc-ctl-added-id = Додано, номер
pc-ctl-pairing-is-open-for = Сполучення відкрите ще
pc-ctl-s = с
pc-ctl-done = Зроблено
pc-ctl-yes = так
pc-ctl-no = ні
pc-ctl-not-asked-yet = ще не спитано
pc-ctl-unknown = невідомо
pc-ctl-not-available = недоступно
pc-ctl-on = увімкнено
pc-ctl-off = вимкнено
pc-ctl-taken-off = , знято
pc-ctl-connected = Підключено
pc-ctl-monitors = моніторів
pc-ctl-waiting-for-the-headset-monitors = Чекає на шолом, монітори лишаються
pc-ctl-waiting-for-a-headset = Чекає на шолом
pc-ctl-power-saving-is-on-for-this = На Wi-Fi цього ПК увімкнено енергозбереження
pc-ctl-it-can-hold-the-picture-back = воно може затримувати картинку на десятки мс. Допоможе кабель або `nmcli connection modify <з'єднання> 802-11-wireless.powersave 2`
pc-ctl-monitor = монітор
pc-ctl-pc-display = екран ПК
pc-ctl-window = вікно
pc-ctl-starting = запускається
pc-ctl-running = працює
pc-ctl-failed = не вдалося
pc-ctl-not-connected = не під'єднано
pc-ctl-kept-for-the-headset = чекає на шолом
pc-ctl-shown = , показано
pc-ctl-hidden = , приховано
pc-ctl-primary = , головний
pc-ctl-frames-s = кадрів/с
pc-ctl-repeated = повторених
pc-ctl-mbit-s = Мбіт/с
pc-ctl-encoder = кодування
pc-ctl-ms = мс
pc-ctl-max = макс.
pc-ctl-guest = гість
pc-ctl-main = головний
pc-ctl-connected-2 = , підключено
pc-ctl-prostir-is-not-running-waiting = Prostir не працює, чекаю
pc-ctl-prostir-is-running = Prostir працює
pc-ctl-headset-connected = Шолом підключився:
pc-ctl-headset-disconnected = Шолом відключився:
pc-ctl-headset-put-on = Шолом вдягнули:
pc-ctl-headset-taken-off = Шолом зняли:
pc-ctl-prostir-did-not-come-on-the = Prostir не з'явився на шині вчасно
pc-ctl-prostir-ended-before-it-started = Prostir завершився, не запустившись

## src/control.rs

pc-control-no-headset-is-connected = Шолом не підключено

## src/main.rs

pc-main-prostir-in-the-hyprland-config = Prostir у конфігу Hyprland
pc-main-change-exec-once-prostir-to-exec = Замініть exec-once = prostir на exec-once = prostir serve: сам prostir тепер відкриває вікно налаштувань.
pc-main-prostir-is-not-running = Prostir не працює
pc-main-port = Порт
pc-main-is-taken-by-another-program = зайнятий іншою програмою.

## src/platform/linux/tray.rs

pc-tray-on = увімкнено
pc-tray-off = вимкнено
pc-tray-headset-taken-off-streaming-paused = Шолом знято, трансляцію призупинено
pc-tray-headset-connected = Шолом підключено
pc-tray-waiting-for-the-headset = Чекаю на шолом…
pc-tray-a-pc-display-in-the-headset = , екран ПК у шоломі
pc-tray-no-virtual-monitors = Віртуальних моніторів немає
pc-tray-virtual-monitors = Віртуальних моніторів
pc-tray-pairing-code = Код сполучення
pc-tray-other-headsets = Інших шоломів
pc-tray-the-desktop-does-not-respond = Стільниця не відповідає
pc-tray-headset-microphone-no-permission = Мікрофон шолома: немає дозволу
pc-tray-headset-battery = Заряд шолома:
pc-tray-charging = , заряджається
pc-tray-bring-the-monitors-in-front = Монітори перед собою
pc-tray-no-limits = Без обмежень
pc-tray-video-quality = Якість відео
pc-tray-sharper-text-about-2-ms-more = Чіткіший текст (кодування на ~2 мс довше)
pc-tray-count-this-pc-among-users = Рахувати цей ПК серед користувачів
pc-tray-start-with-the-system = Запускати разом із системою
pc-tray-settings = Налаштування…
pc-tray-stop-pairing = Зупинити сполучення
pc-tray-pair-a-new-headset = Сполучити новий шолом
pc-tray-forget-paired-headsets = Забути сполучені шоломи
pc-tray-quit = Вийти
pc-tray-quality-economy = Економна: 15 Мбіт/с
pc-tray-quality-normal = Звичайна: 30 Мбіт/с
pc-tray-quality-high = Висока: 50 Мбіт/с

## src/platform/linux/dialogs.rs

pc-dialogs-a-headset-wants-to-connect-type =
    Шолом хоче підключитися.
    Введіть код, який він показує:
pc-dialogs-code-from-the-headset = { "Код із шолома: " }

## src/platform/linux/autostart.rs

pc-autostart-starting-with-hyprland = Запуск разом із Hyprland
pc-autostart-for-prostir-to-wait-for-the = Щоб Prostir чекав на шолом після входу, додайте в конфіг Hyprland рядок: exec-once = prostir serve

## src/platform/local_api.rs

pc-local-api-prostir-is-quitting = Prostir завершує роботу

## src/local.rs

pc-local-a-profile-s-name-is-too = Назва профілю задовга, байтів щонайбільше:
pc-local-the-command-does-not-fit = Команда не годиться
pc-local-it-did-not-work = Не вдалося
pc-local-no-headset-is-connected = Шолом не підключено
pc-local-commands-from-the-pc-go-to = Команди з ПК виконує лише головний шолом, а підключено гостя
pc-local-the-code-six-digits = Код: шість цифр
pc-local-no-headset-is-waiting-for-a = Жоден шолом не чекає на код
pc-local-a-screenshot-is-taken-while-the = Знімок можна зробити, поки монітори є на стільниці
pc-local-the-monitors-left-the-desktop-before = Монітори зникли зі стільниці раніше, ніж знімок
pc-local-quality-5-to-100-mbit-s = Якість: від 5 до 100 Мбіт/с
pc-local-starting-with-the-session-is-not = Тут Prostir не вмикає автозапуск сам
pc-local-the-headset-microphone-is-switched = Мікрофон шолома вмикають лише в шоломі
pc-local-monitors-are-placed-while-they-are = Монітори розставляються, поки вони є на стільниці
pc-local-the-headset-s-volume-is-changed = Гучність шолома змінюється, поки він підключений
pc-local-this-headset-s-volume-is-not = Гучність цього шолома з ПК не змінюється: лише його кнопками
pc-local-no-paired-headset = Немає сполученого шолома
pc-local-several-headsets-have-this-name-give = Так названо кілька шоломів: назвіть ключем
pc-local-the-headset-s-system-does-not = Система шолома не дозволяє записувати мікрофон
pc-local-process = процес
pc-local-prostir-is-already-running = Prostir уже працює
pc-local-but-not-on-this-session-bus = , але не на цій сеансовій шині: його вікно відкривається з сеансу, де його запущено
pc-local-this-prostir-was-started-by-another = цей Prostir запустив інший, який не знайшов запущеного: цей не запускає ще один

## src/pairing.rs

pc-pairing-wrong-pairing-codes = Неправильні коди сполучення
pc-pairing-someone-typed-wrong-codes-several = Хтось кілька разів ввів неправильний код. Нові спроби тепер чекають.
pc-pairing-too-many-wrong-codes-try-again = Забагато неправильних кодів. Спробуйте за
pc-pairing-min = хв.
pc-pairing-another-headset-is-pairing-with-this = Інший шолом саме сполучається з цим ПК. Спробуйте за хвилину.
pc-pairing-no-code-was-typed-on-the = На ПК код не введено
pc-pairing-the-pc-has-no-dialog-for = На ПК немає вікна для коду. Виберіть «Навпаки: код на ноутбуці, ввести тут».
pc-pairing-headset = Шолом
pc-pairing-headset-paired = Шолом сполучено
pc-pairing-can-connect-now = тепер може підключатися.
pc-pairing-pairing-a-headset = Сполучення шолома
pc-pairing-type-this-code-in-the-headset = Введіть у шоломі код
pc-pairing-older = старіша
pc-pairing-update-prostir-on-the-pc = Оновіть Prostir на ПК
pc-pairing-the-headset-app-is-newer = Застосунок у шоломі новіший:
pc-pairing-the-pc-has = а на ПК
pc-pairing-update-prostir-in-the-headset = Оновіть Prostir у шоломі
pc-pairing-the-headset-app-is-older = Застосунок у шоломі старіший:
pc-pairing-the-headset-and-pc-builds-are = Збірки шолома й ПК з різних версій гілки
pc-pairing-the-headset-has = У шоломі
pc-pairing-update-both = Оновіть обидві.

## src/sessions.rs

pc-sessions-this-pc-no-longer-trusts-this = Цей ПК більше не довіряє шолому. Сполучіть їх знову.
pc-sessions-another-headset-is-already-connected = До ПК уже підключено інший шолом.

## src/session/mod.rs

pc-session-headset-battery = Заряд шолома:
pc-session-the-headset-battery-is-low = Шолом скоро розрядиться
pc-session-the-headset-takes-no-commands = Шолом не виконує команд
pc-session-the-headset-does-not-answer = Шолом не відповідає
pc-session-the-headset-disconnected = Шолом відключився
pc-session-the-desktop-does-not-respond = Стільниця не відповідає

## src/wake.rs

pc-wake-new-headset-nearby = Новий шолом поруч
pc-wake-pair-it = Сполучити?
pc-wake-pair = Сполучити
pc-wake-to-pair-it-choose-pair-a = Щоб сполучити, виберіть «Сполучити новий шолом» у меню Prostir.

## src/errors.rs

pc-errors-the-pc-cannot-do-this = ПК цього не вміє
pc-errors-this-headset-is-not-allowed-to = Цьому шолому це не дозволено
pc-errors-no-such-display = Такого екрана немає
pc-errors-the-pc-does-not-make-a = ПК не створює монітор
pc-errors-the-last-monitor-cannot-be-removed = Останній монітор прибрати не можна
pc-errors-no-active-window-on-the-pc = Немає активного вікна на ПК
pc-errors-the-pc-s-desktop-did-not = Стільниця ПК цього не зробила
pc-errors-the-encoder-did-not-start = Кодувальник не запустився
pc-errors-the-picture-could-not-be-taken = Зображення не знято
pc-errors-the-sound-could-not-be-switched = Звук не перемкнувся
pc-errors-the-mouse-is-on-none-of = Миша не на жодному з екранів
pc-errors-a-value-out-of-range = Неправильне значення

## src/displays/arrange.rs

pc-displays-arrange-the-order-does-not-match-the = Порядок не збігається з моніторами

## src/displays/encoding.rs

pc-displays-encoding-more = ще
pc-displays-encoding-is-larger-than-the-headset-decodes = більше, ніж шолом декодує в

## src/displays/parts.rs

pc-displays-parts-more-streams-than-the-headset-takes = більше потоків, ніж шолом бере

## src/displays/sound.rs

pc-displays-sound-the-microphone-is-not-ready-yet = Мікрофон ще не готовий

## src/encoder/choice.rs

pc-encoder-choice-already-has = уже відкрито
pc-encoder-choice-sessions-open-the-most-it-opens = сеансів, більше не відкривається
pc-encoder-choice-did-not-start = не запустився
pc-encoder-cannot-turn-over = ця відеокарта не може перевернути картинку для шолома
pc-encoder-choice-encoding-on-the-cpu = кодування процесором
pc-encoder-choice-frames-go-through-the-cpu = кадри йдуть через процесор

## src/encoder/probe.rs

pc-encoder-probe-no-video-encoder-on-the-pc = На ПК немає відеокодувальника. Intel чи AMD: встановіть VA-плагін GStreamer (gst-plugin-va або gst-plugins-bad) і VA-драйвер. NVIDIA: драйвер NVIDIA і nvcodec з gst-plugins-bad. Будь-яка відеокарта: gst-plugins-ugly (x264, кодування процесором).

## src/worker/run/encoders.rs

pc-worker-run-encoders-encoders-do-not-respond-no-more = кодувальників не відповідають: нових не запускаю
pc-worker-run-encoders-did-not-respond-times = не відповідав разів:
pc-worker-run-encoders-the-source-of-the-frames-fails = джерело кадрів не працює
pc-worker-run-encoders-no-encoder-for-this-output = немає кодувальника для цього виходу
pc-worker-run-encoders-another-program-keeps-changing-the = Інша програма раз у раз змінює розмір монітора
pc-worker-run-encoders-share-an-app-window-in-it = Покажіть у ній вікно програми замість монітора, а тоді зніміть і знову вдягніть шолом.
pc-worker-run-encoders-monitor-size = Розмір монітора
pc-worker-run-encoders-kept = лишився
pc-worker-run-encoders-did-not-start = не запустився

## src/worker/run/outputs.rs

pc-worker-run-outputs-its-output-on-the-desktop-is = його вихід на стільниці зник
pc-worker-run-outputs-a-screen-sharing-program-changed-the = Програма, що показує екран, змінила розмір монітора
pc-worker-run-outputs-its-size-is-back-but-the = Розмір повернено, але показ може стрибати. Щоб цього не було, показуйте вікно програми, а не монітор.
pc-worker-run-outputs-screen-sharing = Показ екрана

## src/worker/run/tests.rs

pc-worker-run-tests-kept-h-264-hevc-did-not = лишився H.264: HEVC не запустився

## src/platform/linux/audio.rs

pc-audio-prostir-headset = Prostir: шолом
pc-audio-prostir-internal = Prostir (службовий)
pc-audio-prostir-headset-microphone = Prostir: мікрофон шолома

## src/platform/sound.rs

# $name: the driver, $url: where to get it, $device: the device to choose
pc-sound-needs-driver = Потрібен драйвер { $name }: { $url }. Після встановлення оберіть у програмах мікрофон «{ $device }».

### Settings window

## Сторінки

window-nav-overview = Огляд
window-nav-monitors = Монітори
window-nav-headsets = Шоломи
window-nav-sound = Звук
window-nav-quality = Якість
window-nav-look = Вигляд
window-quit-prostir = Вийти з Prostir
window-close-note-title = Prostir працює далі
window-close-note = Prostir працює далі у треї. Відкрити знову: значок Prostir у меню або в треї.
window-nav-first-run = Перший запуск

## Рядок стану під сторінками

window-side-loading = Підключення до Prostir
window-side-connected = { $name } підключено
window-side-taken-off = { $name } знято
window-side-connecting = Підключення { $name }
window-side-waiting = Очікування шолома
window-side-failed = Монітор не додано
window-side-not-running = Prostir не запущено
window-side-silent = Prostir не відповідає
window-side-old = Prostir старіший за це вікно
window-side-no-headsets = Шоломів ще немає

## Огляд

window-ov-title = Огляд
window-ov-connected-title = Шолом підключено
window-ov-connected-wifi = { $name } через Wi-Fi. Моніторів у шоломі: { $count }.
window-ov-connected-usb = { $name } через кабель USB. Моніторів у шоломі: { $count }.
window-ov-wifi-power-save = На Wi-Fi цього ПК увімкнено енергозбереження ({ $interface }). Воно може затримувати картинку на десятки мілісекунд. Допоможе кабель або вимкнене енергозбереження для цього з'єднання.
window-ov-taken-off-title = Шолом знято
window-ov-taken-off-body = { $name } на паузі: монітори чекають, поки шолом знову вдягнуть.
window-ov-battery = заряд шолома
window-ov-charging = заряджається
window-ov-fact-sound = Звук ПК
window-ov-fact-mic = Мікрофон шолома
window-ov-fact-profile = Профіль
window-ov-profile-none = Немає
window-ov-connecting-title = Підключення { $name }
window-ov-connecting-body = Шолом знайшов цей ПК. Встановлення захищеного з'єднання.
window-ov-waiting-title = Очікування шолома
window-ov-waiting-body = Відкрийте Prostir у шоломі поруч із цим ПК. Шолом знайде ПК сам, якщо вони в одній мережі.
window-ov-failed-title = Монітор не додано
window-ov-failed-body = { $name }: { $reason } Решта моніторів працює.
window-ov-failed-no-reason = { $name } не додано. Решта моніторів працює.
window-ov-retry = Спробувати ще раз
window-ov-open-monitors = Перейти до моніторів
window-ov-no-headsets-title = Сполучіть перший шолом
window-ov-no-headsets-body = Цей ПК ще не знає жодного шолома. Перший запуск проведе через усе за чотири кроки.
window-ov-no-headsets-go = Почати перший запуск
window-ov-pair-new = Сполучити новий шолом
window-ov-problems = Потребує уваги
window-ov-monitors = Монітори в шоломі
window-ov-monitors-empty = Монітори з'являться тут, коли шолом підключиться.
window-ov-pc = Цей ПК
window-ov-pc-name = Назва в мережі
window-ov-pc-desktop = Стільниця
window-ov-autostart = Запускати разом із системою
window-ov-autostart-desc = Prostir чекатиме на шолом після входу в систему. Вікно при цьому не відкривається.
window-ov-autostart-here-not = На цьому ПК налаштовується разом зі стільницею.
window-ov-counting = Рахувати цей ПК серед користувачів
window-ov-counting-desc = Раз на день один запит, що каже лише, що Prostir використовували: без назви, адреси й налаштувань.

## Питання про підрахунок, поки немає відповіді

window-count-ask-title = Рахувати цей ПК серед користувачів Prostir?
window-count-ask-body = Prostir безкоштовний і без реклами. Число ПК, на яких він працює, це єдиний спосіб зрозуміти, чи варто розвивати його далі. Надсилається лише дата, раз на день (і один раз, що Prostir встановлено), без жодних даних про вас.
window-count-ask-later = Через сервіс GoatCounter. Без відповіді «Так» нічого не надсилається. Змінити відповідь можна будь-коли на сторінці «Огляд» або в треї.
window-count-ask-yes = Так
window-count-ask-no = Ні
window-count-ask-yes-name = Так, рахувати цей ПК
window-count-ask-no-name = Ні, не рахувати цей ПК
window-count-ask-privacy = Політика конфіденційності
window-count-ask-privacy-url = https://prostir.ishawyha.dev/uk/privacy.html#counting

## Стан монітора

window-state-streaming = Передається
window-state-starting = Запускається
window-state-failed = Не додано
window-state-absent = Не під'єднано
window-state-kept = Чекає на шолом
window-state-shown = Показано в шоломі
window-state-hidden = Приховано

## Короткі значення

window-sound-in-headset = У шоломі
window-sound-on-pc = На ПК
window-on = Увімкнено
window-off = Вимкнено
window-percent = { $n } %
window-decimal-separator = ,

## Prostir не запущено

window-loading-title = Підключення до Prostir
window-nr-title = Prostir не запущено
window-nr-body = Налаштування беруться з Prostir, що працює у фоні. Запустіть його, і вікно оновиться само.
window-nr-start = Запустити Prostir
window-nr-starting = Запуск Prostir
window-nr-failed-title = Prostir не запустився
window-nr-failed-body = Запустіть prostir у терміналі, щоб побачити, що йому заважає.
window-nr-no-program = Програми prostir не знайдено. Перевірте, чи встановлено Prostir.
window-nr-old-title = Ця версія Prostir старіша за вікно
window-nr-old-body = Prostir працює, але ця його версія не працює з вікном налаштувань. Оновіть Prostir і спробуйте ще раз.
window-nr-retry = Спробувати ще раз
window-nr-silent-title = Prostir не відповідає
window-nr-silent-body = Prostir працює, але не відповів цьому вікну. Вікно запитує ще раз само.
window-nr-no-bus = Немає сеансової шини D-Bus, тож вікно не може зв'язатися з Prostir.

## Монітори

window-mon-title = Монітори
window-mon-sub = Так їх бачить стільниця ПК: миша переходить між моніторами там, де вони стикаються.
window-mon-flat = Пласка
window-mon-arc = Навколо вас
window-mon-hint-flat = Перетягніть монітор або виберіть його й рухайте стрілками. Він прилипне до сусіда.
window-mon-hint-arc = Так монітори стоять навколо вас. Потягніть порожнє місце, щоб повернути вид.
window-mon-empty = Поки немає моніторів, які можна показати.
window-mon-selected = Вибраний монітор
window-mon-none = Виберіть монітор на карті.
window-mon-size = { $w } × { $h }
window-mon-size-scaled = { $w } × { $h }, масштаб { $scale } %
window-mon-size-at-scale = { $w } × { $h } · { $scale } %
window-mon-scale = Масштаб
window-mon-main = Головний монітор
window-mon-main-desc = Тут панель і нові вікна.
window-mon-remove = Прибрати монітор
window-mon-remove-named = Прибрати { $name }
window-mon-in-headset = У шоломі
window-mon-shown = Показано
window-mon-hidden = Приховано
window-mon-laptop = Екран ноутбука
window-mon-pc-screen = Екран ПК
window-mon-add = Додати монітор
window-mon-add-desc = Новий монітор з'явиться поруч з іншими.
window-mon-placing = { $name }: розставляється…
window-mon-add-full = Більше моніторів не вміщається: у шоломі вже { $max }.
window-mon-add-offline = Монітор з'явиться в шоломі, коли той підключиться.
window-mon-failed = Не додано: { $reason }
window-mon-kept = Чекають на шолом
window-mon-kept-desc = Збережено для наступного шолома: з'являться, коли він підключиться.

## Шоломи

window-hs-title = Шоломи
# $address (String): адреси ПК через кому
window-hs-address = Адреса цього ПК для шолома, що не знаходить його сам: { $address }
window-hs-sub = Шоломи, яким цей ПК довіряє. Новому шолому код потрібен лише раз.
window-hs-now = Підключено зараз
window-hs-last = Востаннє { $when }
window-hs-never = Востаннє: невідомо
window-hs-app = Prostir { $app }, ключ { $key }
window-hs-key = Ключ { $key }
window-ago-now = щойно
window-ago-minutes = { $n } хв тому
window-ago-hours = { $n } год тому
window-ago-days = { $n ->
    [one] { $n } день тому
    [few] { $n } дні тому
   *[many] { $n } днів тому
}
window-hs-forget = Забути
window-hs-forget-named = Забути { $name }
window-hs-forget-ask = Забути { $name }? Щоб підключитися знову, йому знадобиться код.
window-hs-cancel = Скасувати
window-hs-empty-title = Ще немає сполучених шоломів
window-hs-empty-body = Відкрийте Prostir у шоломі поруч із цим ПК і сполучіть їх кодом.
window-hs-pair-hint = Щоб сполучити новий шолом, відкрийте в ньому Prostir поруч із цим ПК: ПК покаже тут код, який треба ввести в шоломі.
window-hs-guest-name = { $name } (гість)
window-hs-profiles = Профілі в { $name }
window-hs-profile-n = Профіль { $n }
window-hs-profile-in-use = Увімкнено зараз
window-hs-profile-rename = Перейменувати
window-hs-profile-rename-named = Перейменувати { $name }
window-hs-profile-name = Нова назва для { $name }
window-hs-profile-save = Зберегти
window-hs-profile-number = Назва не може бути лише числом: число означає місце профілю.
window-hs-profiles-note = Назва з’являється в меню шолома. Порожня назва повертає номер.

## Сполучення

window-pair-title = Сполучення відкрито
window-pair-left = ще { $time }
window-pair-wait = Відкрийте Prostir у новому шоломі поруч із цим ПК. Шолом знайде ПК і покаже код або попросить код звідси.
window-pair-waiting = Очікування нового шолома
window-pair-type = { $name } показує код. Введіть його тут:
window-pair-pc-code = Введіть цей код у шоломі:
window-pair-code = Код із шолома
window-pair-go = Сполучити
window-pair-checking = Перевірка коду
window-pair-stop = Зупинити сполучення
window-pair-wrong = Код не підійшов. Перевірте цифри на шоломі й введіть ще раз.
window-pair-done = { $name } сполучено. Далі він підключатиметься сам.
window-pair-expired = Час сполучення минув. Відкрийте його знову, коли шолом буде поруч.
window-pair-reopen = Відкрити знову
window-pair-close = Закрити
# $address (String): адреси ПК через кому
window-pair-address = Якщо шолом не знаходить ПК сам, введіть у ньому адресу ПК: { $address }
window-pair-firewall = Шолом не бачить ПК? Дозвольте на брандмауері порти 47830/tcp і 47831/udp.

## Звук

window-snd-title = Звук
window-snd-sub = Звук ПК і мікрофон шолома.
window-snd-where = Де грає звук ПК
window-snd-where-desc = У шоломі: колонки ПК мовчать, поки шолом підключено.
window-snd-volume = Гучність
window-snd-volume-desc = Та сама, що на кнопках шолома і в системі.
window-snd-volume-off = Гучність змінюється, коли шолом підключено.
window-snd-volume-headset = Змінюється кнопками шолома.
window-snd-mic = Мікрофон шолома на ПК
window-snd-mic-desc = Програми на ПК бачать його як мікрофон «Prostir».
window-snd-mic-ready = Мікрофон «Prostir» готовий
window-snd-mic-off = Вимкнено: програми на ПК мікрофона шолома не бачать
window-snd-mic-in-headset = Вмикається й вимикається в шоломі.
window-snd-mic-waiting = Мікрофон з'явиться, коли шолом підключиться.
window-snd-mic-not-allowed = Шолом не дозволив мікрофон. Дозвольте його для Prostir у налаштуваннях шолома, серед дозволів програм.
window-snd-mic-failed = Мікрофон не вдалося створити: { $reason }

## Якість

window-q-title = Якість
window-q-sub = Як монітори кодуються для шолома.
window-q-bitrate = Бітрейт
window-q-bitrate-desc = Зараз { $n } Мбіт/с на кожен монітор. Більше: чіткіше, але потрібна швидша мережа.
window-q-mbit = { $n }
window-q-sharp = Чіткіший текст
window-q-sharp-desc = Повільніше кодування, з яким дрібний текст чіткіший.
window-q-codec = Кодек
window-q-codec-desc = HEVC, де ПК і шолом це вміють, інакше H.264.
window-q-cpu = Кодувати процесором
window-q-cpu-desc = Навіть там, де могла б відеокарта.
window-q-using-gpu = Зараз: { $codec } на відеокарті
window-q-using-cpu = Зараз: { $codec } на процесорі
window-q-using-none = Почнеться разом із моніторами.

## Вигляд

window-look-title = Вигляд
window-look-sub = Тема, палітра й мова цього вікна.
window-look-theme = Тема
window-look-system = Як у системі
window-look-light = Світла
window-look-dark = Темна
window-look-now-light = Зараз у системі світла.
window-look-now-dark = Зараз у системі темна.
window-look-now-none = Система не каже, яку тему воліє: показано темну.
window-look-palette = Палітра
window-look-palette-ink = Prostir
window-look-palette-graphite = Нейтральна
window-look-palette-desc = Prostir має відтінок синього. Нейтральна лише сіра, тож нічого не відтінює монітори.
window-look-lang = Мова
window-look-lang-more = Інші мови: покладіть файл .ftl у { $dir }
window-look-local = Prostir не запущено: вибір діє, поки відкрите це вікно.
window-look-file = Файл теми
window-look-file-ok = Застосовано. Зміни у файлі видно одразу, перезапуск не потрібен.
window-look-file-none = Файлу немає: діє вбудована тема. Створіть його, щоб змінити кольори чи розміри.
window-look-file-issues = Ці значення не прочитано, замість них вбудовані:
window-look-file-contrast = Замалий контраст (текст і краї елементів керування):
window-look-create = Створити файл
window-look-open = Відкрити теку
window-issue-not-toml = Файл не читається як TOML: { $why }
window-issue-format = format = { $value }: це вікно читає лише формат 1, тож файл не використано.
window-issue-bad = { $at } = { $value } тут не підходить
window-issue-unknown = { $at }: такого ключа немає
window-issue-font = { $at }: шрифт не читається ({ $why })
window-issue-unreadable = Файл не читається: { $why }
window-contrast-line = { $scheme }: { $fg } на { $bg } дає { $ratio }:1, треба { $need }:1
window-scheme-light = Світла
window-scheme-dark = Темна

## Перший запуск

window-fr-title = Перший запуск
window-fr-step = Крок { $n } з { $total }
window-fr-back = Назад
window-fr-next = Далі
window-fr-done = Готово
window-fr-1-title = Установіть Prostir у шоломі
window-fr-1-body = Увімкніть на шоломі режим розробника, підключіть його кабелем USB і встановіть файл prostir.apk з тієї самої версії, що й на ПК.
window-fr-1-link = Відкрити сторінку версій
window-fr-2-title = Відкрийте Prostir у шоломі поруч із цим ПК
window-fr-2-body = Шолом і ПК мають бути в одній мережі Wi-Fi. Шолом знайде цей ПК сам.
window-fr-2-looking = Пошук шолома в мережі
window-fr-2-found = { $name } бачить цей ПК
window-fr-2-closed = Сполучення закрито, тож новий шолом не знайде цей ПК.
window-fr-2-asks = Шолом попросить код, щойно знайде цей ПК.
window-fr-3-title = Сполучіть кодом
window-fr-3-body = Код потрібен один раз: далі цей шолом підключатиметься сам.
window-fr-3-wait = Коли шолом попросить код, ПК покаже його тут.
window-fr-4-title = Запускати Prostir разом із системою?
window-fr-4-body = Тоді шолом знаходитиме ПК одразу після входу. Без вікон і без зайвої роботи: поки шолома немає, Prostir лише чекає.

## Повідомлення після дії

window-notice-failed = Не вдалося: { $why }
window-notice-timeout = Prostir не відповів вчасно. Спробуйте ще раз.
window-notice-not-running = Prostir не запущено, тож зміну не збережено.
window-notice-broken = Prostir відповів щось незрозуміле для цього вікна: { $why }
window-notice-close = Закрити повідомлення

### Headset app

## Слова з багатьох місць

headset-off = Вимкнено
headset-on = Увімкнено
headset-no = Ні
headset-yes = Так
headset-show = Показувати
headset-hide = Сховати
headset-cancel = Скасувати
headset-connect = Підключитися
headset-menu = Меню
headset-focus = Фокус
headset-cinema = Кінотеатр
headset-line-up = Вирівняти
headset-percent = { $value } %

## Заголовок і вкладки

headset-status-update-needed = Потрібне оновлення
headset-status-pairing = Сполучення…
headset-status-connected = Підключено до { $pc }
headset-status-searching = Шукаю ПК…
headset-change-pc = Змінити ПК
headset-tab-quick = Швидке
headset-tab-monitors = Монітори
headset-tab-profiles = Профілі
headset-tab-settings = Налаштування

## Меню на долоні

headset-wrist-in-front = Перед собою
headset-wrist-mouse = Де миша?
headset-wrist-screenshot = Знімок
headset-battery-level = { $percent } %
headset-battery-charging = { $percent } % · заряджається

## До ПК: привітання і знайдені ПК

headset-welcome-title = Prostir показує монітори вашого ПК
headset-welcome-pc-app = Які ПК підходять: { $site }
headset-welcome-install = Встановіть Prostir на ПК
headset-welcome-start = Запустіть його
headset-welcome-start-note = У треї з'явиться іконка Prostir.
headset-welcome-finds = Шолом знайде ПК сам
headset-welcome-finds-wifi = У тій самій мережі Wi‑Fi.
headset-welcome-finds-wifi-usb = У тій самій мережі Wi‑Fi або через кабель USB.
headset-connect-searching = Шукаю ПК у мережі…
headset-connect-found = Знайдено
headset-found-cable = кабель
headset-connect-blocked = видно в мережі, але з'єднання блокується.
headset-connect-allow-port = Дозвольте в брандмауері ПК порт TCP 47830.
headset-connect-type-ip = Ввести IP вручну
headset-connect-new-title = Не бачу ПК з Prostir
headset-connect-new-pair = На ПК відкрийте меню Prostir і натисніть «Сполучити новий шолом».
headset-connect-or-type = Або введіть адресу ПК: її видно у вікні Prostir на ПК.
headset-connect-or-usb = Або під'єднайте шолом до ПК кабелем USB.
headset-connect-silent-title = ПК не відповідає
headset-connect-silent-check = Перевірте, що Prostir працює на ПК і ПК у тій самій мережі.
# Після назви ПК у списку знайдених
headset-connect-pairing-open = сполучення відкрите
headset-key-delete = Стерти
headset-key-done = Готово

## Сполучення

headset-pairing-with = Сполучення з { $pc }
headset-pairing-failed = Код не підійшов. Спробуйте ще раз
headset-pairing-type-on-pc = Введіть цей код у віконці Prostir на ПК:
headset-pairing-once = Це потрібно лише один раз для кожного ПК.
headset-pairing-other-way-type-here = Навпаки: код на ПК, ввести тут
headset-pairing-type-pc-code = Введіть код, який показує ПК
headset-pairing-where-pc-code = (сповіщення або іконка Prostir у треї):
headset-pairing-other-way-type-on-pc = Навпаки: код у шоломі, ввести на ПК
headset-offer-new = Новий ПК
headset-offer-wrong-codes = Код не підійшов кілька разів
headset-offer-forgotten = ПК більше не знає цей шолом
headset-offer-pc-refused = Сполучення на ПК не почалося
headset-offer-own-pc = Сполучайте шолом лише зі своїм ПК.
headset-offer-code = Потрібно ввести код з одного пристрою на іншому.
headset-offer-pair = Сполучити
headset-offer-try-again = Спробувати ще
headset-offer-pair-again = Сполучити знову
headset-offer-not-now = Не зараз

## Версії, що не з'єднуються

headset-update-headset = Оновіть застосунок у шоломі
headset-update-pc = Оновіть Prostir на ПК
headset-update-versions = Версія на ПК: { $pc }, у шоломі: { $headset }.
headset-update-versions-differ = Різні версії не можуть з'єднатися.
headset-update-pc-from = Завантажте нову версію для ПК з
headset-update-connects-by-itself = Шолом сам підключиться, щойно версії зійдуться.
headset-store-update-library = Оновіть Prostir у бібліотеці шолома:
headset-store-update-apk = Встановіть новий APK з
headset-store-not-confirmed = Не вдалося підтвердити, що Prostir отримано з { $store }

## Поради першого запуску

headset-intro-title = Монітори вже тут. Кілька порад:
headset-intro-menu = Меню
headset-intro-menu-text = Кругла кнопка під головним монітором або кнопка меню чи X на лівому контролері.
headset-intro-move = Перенести монітор
headset-intro-move-text = Тримайте grip і наводьте. Руками: щипок і тримати пів секунди.
headset-intro-line-up = Вирівняти
headset-intro-line-up-text = Кнопка A прибирає шви між моніторами. Відпущений біля іншого монітор пристає сам.
headset-intro-finger = Пальцем
headset-intro-finger-text = Коли руки в полі зору, меню можна просто торкатися, як планшет.
headset-intro-off = Зняли шолом
headset-intro-off-text = Через 10 с вікна, звук і мікрофон повертаються на ПК.
headset-intro-done = Зрозуміло

## Швидке

headset-profiles = Профілі
headset-profile-n = Профіль { $n }
headset-profiles-all = Усі профілі
headset-profiles-none-short = Профілів ще немає
headset-layout-lying = Лежачи
headset-quick-in-front = Перед собою
headset-quick-in-front-state = Повернути все до себе
headset-keyboard-window = Вікно на клавіатуру
headset-pc-sound = Звук ПК
headset-sound-in-headset = У шоломі
headset-sound-on-pc = На ПК
headset-microphone = Мікрофон
headset-quick-mic-headset = Мікрофон шолома
headset-quick-mic-pc = Мікрофон ПК
headset-quick-detach = Відкріпити вікно
headset-quick-detach-full = Уже три: закрийте одне
headset-quick-detach-state = Активне вікно ПК окремо
headset-quick-window-here = Вікно сюди
headset-quick-window-here-state = На монітор перед вами
headset-quick-screenshot = Знімок екрана
headset-quick-screenshot-state = Монітора перед вами

## Монітори

headset-monitor-n = Монітор { $n }
headset-window-n = Вікно { $n }
headset-place-left = ліворуч
headset-place-middle = по центру
headset-place-right = праворуч
headset-place-above = згори
headset-place-below = знизу
headset-monitor-remove-confirm = Прибрати? Ще раз
headset-monitor-primary = Головний
headset-monitor-make-primary = Зробити головним
headset-pc-display = Екран ПК
headset-pc-display-named = Екран { $name }
headset-laptop-screen = Екран ноутбука
headset-pc-display-not-connected = не підключено
headset-pc-display-shown = Показано
headset-pc-display-hidden = Приховано
headset-pc-displays = Екрани ПК
headset-pc-display-limit = досягнуто межі ПК
headset-page-of = Сторінка { $page } з { $pages }
headset-scale = Масштаб
headset-monitors-line-up = Вирівняти: прибрати шви (кнопка A)
headset-add-monitor = Додати монітор
headset-monitors-at-most =
    { $count ->
        [one] Максимум { $count } монітор
        [few] Максимум { $count } монітори
       *[many] Максимум { $count } моніторів
    }
headset-layout = Розкладка
headset-layout-around = Навколо мене
headset-layout-wall = Стіна
headset-curve = Вигин
headset-curve-flat = Пласкі
headset-curve-gentle = Легкий
headset-curve-strong = Сильний
headset-follow-head = Слідом за головою
headset-magnet = Прилипання
headset-drag = Тягнути
headset-drag-one = Один монітор
headset-drag-all = Усі разом
headset-cinema-screen = Екран для кінотеатру
headset-profile-replace = Замінити?

## Профілі

headset-profile-new = Новий профіль
headset-profile-save-here = Зберегти сюди
headset-profile-delete-confirm = Видалити? Ще раз
headset-profiles-none = Розставте монітори й натисніть «Новий профіль»: розстановка збережеться, і до неї можна повернутися одним натисканням.
headset-profiles-at-most =
    { $count ->
        [one] Щонайбільше { $count } профіль
        [few] Щонайбільше { $count } профілі
       *[many] Щонайбільше { $count } профілів
    }
headset-profiles-rename-hint = Назву профілю можна змінити у вікні Prostir на ПК.
headset-note-profile-deleted = Профіль «{ $name }» видалено
headset-note-profiles-full =
    { $count ->
        [one] Уже { $count } профіль: спершу видаліть зайвий
        [few] Уже { $count } профілі: спершу видаліть зайвий
       *[many] Уже { $count } профілів: спершу видаліть зайвий
    }

## Налаштування

headset-section-look = Вигляд
headset-section-controls = Керування
headset-section-sound = Звук
headset-section-language = Мова
headset-section-pc = ПК
headset-section-paired = Сполучені ПК
headset-section-help = Довідка

## Налаштування: вигляд

headset-background = Фон
headset-background-black = Чорний
headset-background-room = Кімната
headset-background-dimmed = Притемнена
headset-background-own = Своє фото
headset-background-own-where = Своє фото: панорама 360° у файлі
headset-sky-night = Ніч
headset-sky-sunset = Захід
headset-sky-space = Космос
headset-sky-aurora = Сяйво
headset-focus-dim-others = Затемнювати інші
headset-focus-note = Монітори, на яких немає миші, темнішають.
headset-keyboard-window-note = Видно справжній стіл; поставте його на клавіатуру.
headset-rest = Нагадування про відпочинок
headset-rest-every-20 = Кожні 20 хв
headset-rest-hourly = Щогодини
headset-brightness = Яскравість моніторів

## Налаштування: керування

headset-hands = Руки
headset-hands-use = І керувати
headset-gaze-mouse = Миша за поглядом
headset-gaze-mouse-note = Подивились на інший монітор: миша переходить туди.
headset-gaze-mouse-unavailable = Цей ПК не дає переміщати мишу.
headset-menu-size = Розмір меню
headset-menu-size-normal = Звичайний
headset-menu-size-large = Великий
headset-palm-menu = Меню на долоні
headset-palm-menu-left = На лівій руці
headset-palm-menu-right = На правій руці
headset-display-rate = Частота екрана шолома
headset-display-rate-normal = { $hz } Гц
headset-display-rate-saver = { $hz } Гц, довше від заряду
headset-display-rate-note = Відео йде 60 кадрів на секунду, тож { $hz } Гц його не обрізає.
headset-display-rate-now = Зараз екран показує { $hz } Гц.
headset-menu-button = Кругла кнопка меню біля монітора

## Налаштування: звук

headset-sound-plays = Звук ПК грає
headset-sound-apps-hear = Програми на ПК чують
headset-sound-mic-headset = Мікрофон шолома
headset-sound-mic-pc = Мікрофон ПК
headset-sound-mic-denied-1 = Prostir не має доступу до мікрофона. Дозвольте його
headset-sound-mic-denied-2 = в налаштуваннях шолома (дозволи застосунків).
headset-sound-ask-again = Спитати ще раз
headset-sound-from-screen = Звук з боку екрана
headset-sound-from-screen-note = Звук лунає з того монітора, де вікно, яке грає.
headset-sound-about-1 = Поки шолом підключено, звук із ПК грає в шоломі,
headset-sound-about-2 = а програми на ПК чують мікрофон шолома.
headset-sound-about-3 = Мікрофон вмикається, лише коли програма записує.
headset-sound-about-4 = Коли шолом знято, звук і мікрофон повертаються на ПК.

## Налаштування: мова, час і дата

headset-language-title = Мова меню · Language
headset-language-like-pc = Як на ПК
headset-language-like-system = Як у системі
headset-language-more = Більше…
headset-language-problems = Деякі рядки файлу мови { $language } не прочитано: вони англійською
headset-language-missing = Немає файлу мови { $language }: меню англійською
headset-time = Час
headset-date = Дата
headset-date-like-menu = Мовою меню

## Налаштування: ПК

headset-pc-not-connected = Доступно, коли ПК підключено.
headset-pc-quality = Якість відео
headset-pc-quality-economy = Економна
headset-pc-quality-normal = Звичайна
headset-pc-quality-high = Висока
headset-pc-encoding-on = Кодує
headset-pc-gpu = Відеокарта
headset-pc-cpu = Процесор
headset-pc-codec = Кодек
headset-pc-codec-note = HEVC: найчіткіший текст. H.264: менше пам'яті на ПК.
headset-pc-sharp = Чіткіший текст (+2 мс)
headset-pc-notifications = Сповіщення ПК у шоломі
headset-no-limits = Без обмежень
headset-no-limits-note = Більше моніторів і екранів, ніж радить Prostir. Можливі пропуски кадрів, нагрів і швидша розрядка.
headset-paired-pcs = Сполучені ПК
headset-paired-none = Немає
headset-paired-forget-confirm = Забути? Ще раз
headset-paired-forget = Забути
headset-paired-more = І ще { $count }

## Налаштування: довідка

headset-hint-finger = Пальцем: торкніться кнопки або тягніть меню за порожнє місце
headset-hint-hand = Здалеку рукою: щипок натискає, щипок з утриманням переносить
headset-hint-controller = Контролер: курок натискає, grip переносить
headset-hint-stick = Стік, поки тримаєте: відстань і розмір. A: вирівняти
headset-help-show-tips = Показати підказки знову

## Біля моніторів

headset-attach = Прикріпити
headset-adding-monitor = Монітор додається…

## Повідомлення

headset-ask-waiting = Зачекайте…
headset-rest-eyes = Перепочиньте очі: 20 секунд подивіться вдалину
headset-rest-hour = Година в шоломі: час на коротку перерву
headset-toast-screenshot-saved = Знімок збережено: { $path }
headset-toast-profile-saved = Збережено в «{ $name }»
headset-toast-profile-switched = Увімкнено «{ $name }»
headset-error-send = Не вдалося надіслати на ПК: { $error }
headset-error-not-connected = Немає з'єднання з ПК
headset-error-too-big = { $name }: { $size } ({ $codec }) завеликий, щоб цей шолом його показав
headset-error-cameras = Камери недоступні: { $error }
headset-error-cameras-lost = Камери перестали працювати: «Кімната» і «Притемнена» повернуться після наступного запуску застосунку.
headset-error-keyboard-window = Вікно на клавіатуру зараз не вдається показати: після наступного запуску застосунку буде ще одна спроба.
headset-layers-hidden = { $count ->
    [one] Забагато, щоб показати все разом: { $count } найдальший монітор поки сховано
    [few] Забагато, щоб показати все разом: { $count } найдальші монітори поки сховано
   *[many] Забагато, щоб показати все разом: { $count } найдальших моніторів поки сховано
}
headset-video-trouble = Немає картинки, відновлення…
headset-video-no-decoder = цей шолом не має декодера { $codec }
headset-video-bad-size = цей шолом не декодує { $codec } розміру { $size }
headset-video-no-surface = шолом не зміг показати цю картинку
headset-video-stopped = декодер відео зупинився
headset-video-undecodable = шолом не зміг декодувати цю картинку
headset-video-not-shown = Не показано: { $reason }
headset-video-failed = { $name } не показано: { $reason }
headset-error-not-ip = «{ $typed }»: не IP-адреса
headset-error-save-settings = Не вдалося зберегти налаштування: { $error }
headset-error-save-desk = Не вдалося зберегти розстановку: { $error }
headset-error-save-profiles = Не вдалося зберегти профілі: { $error }
headset-error-save-keyboard = Не вдалося зберегти місце вікна на клавіатуру: { $error }
headset-error-save-files = Не вдалося зберегти зміни: { $error }
headset-pc-usb = ПК (USB)
headset-bye-pc-quit = Програму на ПК закрито
headset-bye-forgotten = ПК більше не знає цей шолом. Сполучіть знову
headset-bye-pc-ended = ПК завершив з'єднання
headset-bye-silence = Немає зв'язку з ПК

## Під кнопкою, яку натиснули в меню

headset-note-monitor-added = Монітор додано
headset-note-monitor-removed = Монітор прибрано
headset-note-window-closed = Вікно повернуто на монітори
headset-note-primary = Тепер це головний монітор
headset-note-display-shown = Екран ПК показано
headset-note-display-hidden = Екран ПК приховано
headset-note-scale = Масштаб змінено
headset-note-no-limits-on = Можна більше моніторів і екранів
headset-note-no-limits-off = Знову рекомендовані межі
headset-note-sound-headset = Звук ПК грає в шоломі
headset-note-sound-pc = Звук ПК грає на ПК
headset-note-encoding = Налаштування відео застосовано
headset-note-window-detached = Вікно відкріплено
headset-note-window-here = Вікно перенесено сюди
headset-note-layout = Монітори розставлено заново
headset-note-lined-up = Шви прибрано
headset-note-in-front = Усе перед вами
headset-note-mic-headset = Програми на ПК чують мікрофон шолома
headset-note-mic-pc = Програми на ПК чують мікрофон ПК
headset-note-mic-ask = Дозвіл на мікрофон спитає система
headset-note-mic-allowed = Доступ до мікрофона дозволено
headset-note-mic-denied = Немає доступу до мікрофона
headset-note-language = Мову меню змінено
headset-note-pc-forgotten = { $pc } прибрано зі сполучених ПК
headset-note-paired = Сполучено з { $pc }
headset-note-no-monitor-in-front = Перед вами немає монітора

## Що відповів ПК, коли запит не вдався

headset-failure-not-connected = Немає з'єднання з ПК
headset-failure-disconnected = З'єднання з ПК обірвалося
headset-failure-timed-out = ПК не відповідає
headset-failure-unsupported = ПК цього не вміє
headset-failure-not-allowed = Цьому шолому не дозволено
headset-failure-unknown-display = Такого екрана вже немає
headset-failure-limit = Досягнуто межі
headset-failure-bad-size = Розмір, якого ПК не робить або шолом не покаже
headset-failure-last-display = Останній монітор лишається
headset-failure-no-window = На ПК немає активного вікна
headset-failure-desktop = Стільниця ПК цього не зробила
headset-failure-encoder = Кодувальник ПК не запустився
headset-failure-capture = Зображення не знято
headset-failure-sound = Звук не перемкнувся
headset-failure-not-on-displays = Миша не на моніторах
headset-failure-no-such-profile = Такого профілю немає
headset-failure-bad-argument = Неправильне значення
headset-failure-other = Не вдалося

## Дата й 12-годинний годинник на долоні

headset-date-format = { $weekday }, { $day } { $month }
headset-weekday-0 = Нд
headset-weekday-1 = Пн
headset-weekday-2 = Вт
headset-weekday-3 = Ср
headset-weekday-4 = Чт
headset-weekday-5 = Пт
headset-weekday-6 = Сб
headset-month-0 = січ.
headset-month-1 = лют.
headset-month-2 = бер.
headset-month-3 = квіт.
headset-month-4 = трав.
headset-month-5 = черв.
headset-month-6 = лип.
headset-month-7 = серп.
headset-month-8 = вер.
headset-month-9 = жовт.
headset-month-10 = лист.
headset-month-11 = груд.
headset-time-12h =
    { $half ->
        [am] { $time } AM
       *[pm] { $time } PM
    }
