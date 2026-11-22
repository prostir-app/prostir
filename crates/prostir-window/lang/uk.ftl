# Вікно налаштувань Prostir, українською. Чого тут бракує, береться з en.ftl.

language-name = Українська

## Сторінки

nav-overview = Огляд
nav-monitors = Монітори
nav-headsets = Шоломи
nav-sound = Звук
nav-quality = Якість
nav-look = Вигляд
quit-prostir = Вийти з Prostir
close-note-title = Prostir працює далі
close-note = Prostir працює далі у треї. Відкрити знову: значок Prostir у меню або в треї.
nav-first-run = Перший запуск

## Рядок стану під сторінками

side-loading = Підключення до Prostir
side-connected = { $name } підключено
side-taken-off = { $name } знято
side-connecting = Підключення { $name }
side-waiting = Очікування шолома
side-failed = Монітор не додано
side-not-running = Prostir не запущено
side-silent = Prostir не відповідає
side-old = Prostir старіший за це вікно
side-no-headsets = Шоломів ще немає

## Огляд

ov-title = Огляд
ov-connected-title = Шолом підключено
ov-connected-wifi = { $name } через Wi-Fi. Моніторів у шоломі: { $count }.
ov-connected-usb = { $name } через кабель USB. Моніторів у шоломі: { $count }.
ov-wifi-power-save = На Wi-Fi цього ПК увімкнено енергозбереження ({ $interface }). Воно може затримувати картинку на десятки мілісекунд. Допоможе кабель або вимкнене енергозбереження для цього з'єднання.
ov-taken-off-title = Шолом знято
ov-taken-off-body = { $name } на паузі: монітори чекають, поки шолом знову вдягнуть.
ov-battery = заряд шолома
ov-charging = заряджається
ov-fact-sound = Звук ПК
ov-fact-mic = Мікрофон шолома
ov-fact-profile = Профіль
ov-profile-none = Немає
ov-connecting-title = Підключення { $name }
ov-connecting-body = Шолом знайшов цей ПК. Встановлення захищеного з'єднання.
ov-waiting-title = Очікування шолома
ov-waiting-body = Відкрийте Prostir у шоломі поруч із цим ПК. Шолом знайде ПК сам, якщо вони в одній мережі.
ov-failed-title = Монітор не додано
ov-failed-body = { $name }: { $reason } Решта моніторів працює.
ov-failed-no-reason = { $name } не додано. Решта моніторів працює.
ov-retry = Спробувати ще раз
ov-open-monitors = Перейти до моніторів
ov-no-headsets-title = Сполучіть перший шолом
ov-no-headsets-body = Цей ПК ще не знає жодного шолома. Перший запуск проведе через усе за чотири кроки.
ov-no-headsets-go = Почати перший запуск
ov-pair-new = Сполучити новий шолом
ov-problems = Потребує уваги
ov-monitors = Монітори в шоломі
ov-monitors-empty = Монітори з'являться тут, коли шолом підключиться.
ov-pc = Цей ПК
ov-pc-name = Назва в мережі
ov-pc-desktop = Стільниця
ov-autostart = Запускати разом із системою
ov-autostart-desc = Prostir чекатиме на шолом після входу в систему. Вікно при цьому не відкривається.
ov-autostart-here-not = На цьому ПК налаштовується разом зі стільницею.
ov-counting = Рахувати цей ПК серед користувачів
ov-counting-desc = Раз на день один запит, що каже лише, що Prostir використовували: без назви, адреси й налаштувань.

## Питання про підрахунок, поки немає відповіді

count-ask-title = Рахувати цей ПК серед користувачів Prostir?
count-ask-body = Prostir безкоштовний і без реклами. Число ПК, на яких він працює, це єдиний спосіб зрозуміти, чи варто розвивати його далі. Надсилається лише дата, раз на день (і один раз, що Prostir встановлено), без жодних даних про вас.
count-ask-later = Через сервіс GoatCounter. Без відповіді «Так» нічого не надсилається. Змінити відповідь можна будь-коли на сторінці «Огляд» або в треї.
count-ask-yes = Так
count-ask-no = Ні
count-ask-yes-name = Так, рахувати цей ПК
count-ask-no-name = Ні, не рахувати цей ПК
count-ask-privacy = Політика конфіденційності
count-ask-privacy-url = https://prostir.ishawyha.dev/uk/privacy.html#counting

## Стан монітора

state-streaming = Передається
state-starting = Запускається
state-failed = Не додано
state-absent = Не під'єднано
state-kept = Чекає на шолом
state-shown = Показано в шоломі
state-hidden = Приховано

## Короткі значення

sound-in-headset = У шоломі
sound-on-pc = На ПК
on = Увімкнено
off = Вимкнено
percent = { $n } %
decimal-separator = ,

## Prostir не запущено

loading-title = Підключення до Prostir
nr-title = Prostir не запущено
nr-body = Налаштування беруться з Prostir, що працює у фоні. Запустіть його, і вікно оновиться само.
nr-start = Запустити Prostir
nr-starting = Запуск Prostir
nr-failed-title = Prostir не запустився
nr-failed-body = Запустіть prostir у терміналі, щоб побачити, що йому заважає.
nr-no-program = Програми prostir не знайдено. Перевірте, чи встановлено Prostir.
nr-old-title = Ця версія Prostir старіша за вікно
nr-old-body = Prostir працює, але ця його версія не працює з вікном налаштувань. Оновіть Prostir і спробуйте ще раз.
nr-retry = Спробувати ще раз
nr-silent-title = Prostir не відповідає
nr-silent-body = Prostir працює, але не відповів цьому вікну. Вікно запитує ще раз само.
nr-no-bus = Немає сеансової шини D-Bus, тож вікно не може зв'язатися з Prostir.

## Монітори

mon-title = Монітори
mon-sub = Так їх бачить стільниця ПК: миша переходить між моніторами там, де вони стикаються.
mon-flat = Пласка
mon-arc = Навколо вас
mon-hint-flat = Перетягніть монітор або виберіть його й рухайте стрілками. Він прилипне до сусіда.
mon-hint-arc = Так монітори стоять навколо вас. Потягніть порожнє місце, щоб повернути вид.
mon-empty = Поки немає моніторів, які можна показати.
mon-selected = Вибраний монітор
mon-none = Виберіть монітор на карті.
mon-size = { $w } × { $h }
mon-size-scaled = { $w } × { $h }, масштаб { $scale } %
mon-size-at-scale = { $w } × { $h } · { $scale } %
mon-scale = Масштаб
mon-main = Головний монітор
mon-main-desc = Тут панель і нові вікна.
mon-remove = Прибрати монітор
mon-remove-named = Прибрати { $name }
mon-in-headset = У шоломі
mon-shown = Показано
mon-hidden = Приховано
mon-laptop = Екран ноутбука
mon-pc-screen = Екран ПК
mon-add = Додати монітор
mon-add-desc = Новий монітор з'явиться поруч з іншими.
mon-placing = { $name }: розставляється…
mon-add-full = Більше моніторів не вміщається: у шоломі вже { $max }.
mon-add-offline = Монітор з'явиться в шоломі, коли той підключиться.
mon-failed = Не додано: { $reason }
mon-kept = Чекають на шолом
mon-kept-desc = Збережено для наступного шолома: з'являться, коли він підключиться.

## Шоломи

hs-title = Шоломи
# $address (String): адреси ПК через кому
hs-address = Адреса цього ПК для шолома, що не знаходить його сам: { $address }
hs-sub = Шоломи, яким цей ПК довіряє. Новому шолому код потрібен лише раз.
hs-now = Підключено зараз
hs-last = Востаннє { $when }
hs-never = Востаннє: невідомо
hs-app = Prostir { $app }, ключ { $key }
hs-key = Ключ { $key }
ago-now = щойно
ago-minutes = { $n } хв тому
ago-hours = { $n } год тому
ago-days = { $n ->
    [one] { $n } день тому
    [few] { $n } дні тому
   *[many] { $n } днів тому
}
hs-forget = Забути
hs-forget-named = Забути { $name }
hs-forget-ask = Забути { $name }? Щоб підключитися знову, йому знадобиться код.
hs-cancel = Скасувати
hs-empty-title = Ще немає сполучених шоломів
hs-empty-body = Відкрийте Prostir у шоломі поруч із цим ПК і сполучіть їх кодом.
hs-pair-hint = Щоб сполучити новий шолом, відкрийте в ньому Prostir поруч із цим ПК: ПК покаже тут код, який треба ввести в шоломі.
hs-guest-name = { $name } (гість)

## Сполучення

pair-title = Сполучення відкрито
pair-left = ще { $time }
pair-wait = Відкрийте Prostir у новому шоломі поруч із цим ПК. Шолом знайде ПК і покаже код або попросить код звідси.
pair-waiting = Очікування нового шолома
pair-type = { $name } показує код. Введіть його тут:
pair-pc-code = Введіть цей код у шоломі:
pair-code = Код із шолома
pair-go = Сполучити
pair-checking = Перевірка коду
pair-stop = Зупинити сполучення
pair-wrong = Код не підійшов. Перевірте цифри на шоломі й введіть ще раз.
pair-done = { $name } сполучено. Далі він підключатиметься сам.
pair-expired = Час сполучення минув. Відкрийте його знову, коли шолом буде поруч.
pair-reopen = Відкрити знову
pair-close = Закрити
# $address (String): адреси ПК через кому
pair-address = Якщо шолом не знаходить ПК сам, введіть у ньому адресу ПК: { $address }
pair-firewall = Шолом не бачить ПК? Дозвольте на брандмауері порти 47830/tcp і 47831/udp.

## Звук

snd-title = Звук
snd-sub = Звук ПК і мікрофон шолома.
snd-where = Де грає звук ПК
snd-where-desc = У шоломі: колонки ПК мовчать, поки шолом підключено.
snd-volume = Гучність
snd-volume-desc = Та сама, що на кнопках шолома і в системі.
snd-volume-off = Гучність змінюється, коли шолом підключено.
snd-volume-headset = Змінюється кнопками шолома.
snd-mic = Мікрофон шолома на ПК
snd-mic-desc = Програми на ПК бачать його як мікрофон «Prostir».
snd-mic-ready = Мікрофон «Prostir» готовий
snd-mic-off = Вимкнено: програми на ПК мікрофона шолома не бачать
snd-mic-in-headset = Вмикається й вимикається в шоломі.
snd-mic-waiting = Мікрофон з'явиться, коли шолом підключиться.
snd-mic-not-allowed = Шолом не дозволив мікрофон. Дозвольте його для Prostir у налаштуваннях шолома, серед дозволів програм.
snd-mic-failed = Мікрофон не вдалося створити: { $reason }

## Якість

q-title = Якість
q-sub = Як монітори кодуються для шолома.
q-bitrate = Бітрейт
q-bitrate-desc = Зараз { $n } Мбіт/с на кожен монітор. Більше: чіткіше, але потрібна швидша мережа.
q-mbit = { $n }
q-sharp = Чіткіший текст
q-sharp-desc = Повільніше кодування, з яким дрібний текст чіткіший.
q-codec = Кодек
q-codec-desc = HEVC, де ПК і шолом це вміють, інакше H.264.
q-cpu = Кодувати процесором
q-cpu-desc = Навіть там, де могла б відеокарта.
q-using-gpu = Зараз: { $codec } на відеокарті
q-using-cpu = Зараз: { $codec } на процесорі
q-using-none = Почнеться разом із моніторами.

## Вигляд

look-title = Вигляд
look-sub = Тема, палітра й мова цього вікна.
look-theme = Тема
look-system = Як у системі
look-light = Світла
look-dark = Темна
look-now-light = Зараз у системі світла.
look-now-dark = Зараз у системі темна.
look-now-none = Система не каже, яку тему воліє: показано темну.
look-palette = Палітра
look-palette-ink = Prostir
look-palette-graphite = Нейтральна
look-palette-desc = Prostir має відтінок синього. Нейтральна лише сіра, тож нічого не відтінює монітори.
look-lang = Мова
look-lang-more = Інші мови: покладіть файл .ftl у { $dir }
look-local = Prostir не запущено: вибір діє, поки відкрите це вікно.
look-file = Файл теми
look-file-ok = Застосовано. Зміни у файлі видно одразу, перезапуск не потрібен.
look-file-none = Файлу немає: діє вбудована тема. Створіть його, щоб змінити кольори чи розміри.
look-file-issues = Ці значення не прочитано, замість них вбудовані:
look-file-contrast = Замалий контраст (текст і краї елементів керування):
look-create = Створити файл
look-open = Відкрити теку
issue-not-toml = Файл не читається як TOML: { $why }
issue-format = format = { $value }: це вікно читає лише формат 1, тож файл не використано.
issue-bad = { $at } = { $value } тут не підходить
issue-unknown = { $at }: такого ключа немає
issue-font = { $at }: шрифт не читається ({ $why })
issue-unreadable = Файл не читається: { $why }
contrast-line = { $scheme }: { $fg } на { $bg } дає { $ratio }:1, треба { $need }:1
scheme-light = Світла
scheme-dark = Темна

## Перший запуск

fr-title = Перший запуск
fr-step = Крок { $n } з { $total }
fr-back = Назад
fr-next = Далі
fr-done = Готово
fr-1-title = Установіть Prostir у шоломі
fr-1-body = Увімкніть на шоломі режим розробника, підключіть його кабелем USB і встановіть файл prostir.apk з тієї самої версії, що й на ПК.
fr-1-link = Відкрити сторінку версій
fr-2-title = Відкрийте Prostir у шоломі поруч із цим ПК
fr-2-body = Шолом і ПК мають бути в одній мережі Wi-Fi. Шолом знайде цей ПК сам.
fr-2-looking = Пошук шолома в мережі
fr-2-found = { $name } бачить цей ПК
fr-2-closed = Сполучення закрито, тож новий шолом не знайде цей ПК.
fr-2-asks = Шолом попросить код, щойно знайде цей ПК.
fr-3-title = Сполучіть кодом
fr-3-body = Код потрібен один раз: далі цей шолом підключатиметься сам.
fr-3-wait = Коли шолом попросить код, ПК покаже його тут.
fr-4-title = Запускати Prostir разом із системою?
fr-4-body = Тоді шолом знаходитиме ПК одразу після входу. Без вікон і без зайвої роботи: поки шолома немає, Prostir лише чекає.

## Повідомлення після дії

notice-failed = Не вдалося: { $why }
notice-timeout = Prostir не відповів вчасно. Спробуйте ще раз.
notice-not-running = Prostir не запущено, тож зміну не збережено.
notice-broken = Prostir відповів щось незрозуміле для цього вікна: { $why }
notice-close = Закрити повідомлення
