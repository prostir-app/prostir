# Внески в інші проєкти: правила і стан (дослідження 2026-11-25)

Мета: зрозуміти, куди і як власник Prostir може внести віртуальні виходи та захоплення.
Контекст: architecture-0.2.0.md, розділ «Відкрита розробка на GitHub і внески в інші
проєкти», і рядок «Перевірено 2026-11-20» (COSMIC, driftwm, niri, mangowm).
Нічого ніде не публікували. Лише читання сайтів і репозиторіїв.

Головне коротко:

- driftwm уже має `ext-image-copy-capture-v1` для виходів. Бракує лише створення
  віртуального виходу. Автор сам прийняв цю ідею в issue #180 і #202. ШІ дозволено.
- niri, COSMIC, WiVRn забороняють текст і код від ШІ. Там пише лише власник.
- xdg-desktop-portal уже має тип джерела VIRTUAL у ScreenCast, але без розміру й частоти.
  Обговорення динамічних віртуальних моніторів є (#1872), єдиної думки немає.
- Monado не має письмової політики щодо ШІ, але вимагає DCO, який «може засвідчити лише
  людина».

## Політика щодо ШІ одним рядком

| Проєкт | Політика | Хто пише текст і код |
|---|---|---|
| driftwm | Дозволено, проєкт сам «AI-assisted», є AGENTS.md для агентів | Я, власник перевіряє й пушить |
| niri | Заборонено створювати ШІ код, тести, issue, коментарі, рецензії; переклад дозволено з оригіналом | Лише власник |
| COSMIC (cosmic-comp) | Заборонено будь-який вміст від ШІ в issue і PR | Лише власник |
| xdg-desktop-portal | Письмової політики не знайшли | Краще власник (рецензенти з GNOME і KDE) |
| Monado | Письмової політики не знайшли; DCO лише від людини | Власник, з `Signed-off-by` |
| WiVRn | Код від машини не приймають, крім дрібниць; AGENTS.md забороняє агентам PR і issue | Лише власник |

## driftwm

Репозиторій: https://github.com/malbiruk/driftwm (дзеркало на Codeberg).
Клон для читання: /home/ishawyha/Documents/Coding/contrib/driftwm (50 комітів, версія 0.21.1).

| Що | Подробиці |
|---|---|
| Правила | [CONTRIBUTING.md](https://github.com/malbiruk/driftwm/blob/main/CONTRIBUTING.md): для нетривіальних змін спершу issue; малі сфокусовані PR (одна зміна на PR); CI: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test -- --include-ignored`; локально `dev/scripts/ci-local`. Перед кодом прочитати `dev/docs/caveats.md` і `dev/docs/testing.md`. У `src/state/mod.rs` лише стан, методи в сусідньому модулі. Патчі також поштою через `git format-patch`. Шаблонів PR і issue немає, CLA і DCO немає, кодексу поведінки немає. |
| Стиль комітів | Англійською, наказовий спосіб, з великої літери, без префікса й крапки («Refuse an IPC move on a window that is not placed yet»); при злитті додається `(#343)`; «closes #325» у заголовку буває. |
| Політика ШІ | Дозволено. Цитата: «driftwm is experimental and primarily an AI-assisted learning project, but PRs and issues are welcome.» І: «Using an AI coding agent? AGENTS.md is the single source of agent instructions.» ([CONTRIBUTING](https://github.com/malbiruk/driftwm/blob/main/CONTRIBUTING.md), [AGENTS.md](https://github.com/malbiruk/driftwm/blob/main/AGENTS.md)). Розкривати не вимагають. |
| Стан | Є `ext-image-copy-capture-v1`, `ext-image-capture-source-v1` (вихід і вікно), `wlr-screencopy`, `wlr-output-management`; трансляції через xdg-desktop-portal-wlr (від 0.8.0) або -cosmic. Віртуальних виходів немає. Автор прийняв [#180 «Virtual Display/Canvas Regions»](https://github.com/malbiruk/driftwm/issues/180) (2026-07-06): «a fixed rectangle of the canvas that appears to OBS/Discord as if it were a monitor», через IPC `driftwm msg virtual-display create -g <geometry>` / `remove`, «on the roadmap», «won't be immediate». Також прийняв [#202 «Headless mode»](https://github.com/malbiruk/driftwm/issues/202): бракує «an offscreen renderer + frame loop», «likely lands together with #180». Відкритий [PR #348](https://github.com/malbiruk/driftwm/pull/348) чіпає створення `wl_output` у udev.rs (6 рядків). |
| Хто і як активно | Один автор (malbiruk), коміти щодня, близько 1930 зірок. Сторонні PR зливає за 1 до 20 днів (#343: 11 днів, #319: 1 день, #296: 22 дні). |
| Перший крок | Коментар у #180 (і посилання з #202): варіант для VR, питання про задум (нижче). PR лише після відповіді. |

### Архітектура driftwm, що стосується нас

- Smithay з git, rev `4cf0b62` (2026-08-12), версія 0.7.0. Функції: winit, udev, drm,
  gbm, libinput, libseat, egl, renderer_gl, wayland_frontend, desktop.
- Бекенди: `src/backend/winit.rs` (вкладений, один вихід «winit», таймер ~60 Гц) і
  `src/backend/udev.rs` (DRM). У udev кожен вихід це `SurfaceData` з `DrmCompositor`, ключ
  `crtc::Handle`. Створення в `create_surface`, знищення в `teardown_output`, малювання в
  `render_if_needed` → `render_frame`.
- Спільне для всіх бекендів: `DriftWm::output_connected` і `output_disconnected`
  (`src/state/output.rs`): позиція, свій вид полотна (камера, масштаб), фокус, `Space`.
- Захоплення не залежить від DRM. `render_capture_frames`, `render_screencopy`
  (`src/render/capture.rs`) малюють готовий список елементів у буфер клієнта (dmabuf чи
  shm) зі своїм відстеженням пошкоджень. Список дає `compose_frame` (`src/render/mod.rs`).
  Тобто віртуальному виходу не потрібен вивід на екран, лише цикл кадрів.
- Протокол `ext-image-copy-capture` у driftwm свій (`src/protocols/image_copy_capture.rs`),
  джерело з `smithay::wayland::image_capture_source`. У Smithay на цьому rev є і свій
  модуль `image_copy_capture`, але driftwm його не бере.
- Smithay не має headless бекенда (немає `backend::headless`). Віртуальний вихід у Smithay
  це `Output::new` + глобаль `wl_output` + свій таймер + малювання поза екраном. Так само
  роблять тести driftwm: `src/tests/headless.rs`, `add_output_full` створює вихід
  `HEADLESS-n` без бекенда, і ядро з ним працює.
- IPC: Unix-сокет, JSON по рядку. `src/ipc/protocol.rs` (enum `Request`, `Response`),
  `src/ipc/mod.rs` (обробники `cmd_*`, `is_mutating`), `src/ipc/client.rs` (підкоманди
  clap для `driftwm msg`), опис у `docs/ipc.md`. Команд для виходів немає.

### Мінімальна зміна: віртуальний вихід WxH@Hz через IPC

Що зробити:

1. Новий модуль, наприклад `src/backend/virtual_output.rs` (або метод `impl DriftWm` у
   `src/state/virtual_output.rs`, поле `virtual_outputs` у `src/state/mod.rs`):
   `Output::new` (make «driftwm», model «virtual», ім'я `VIRTUAL-n` або задане), режим,
   масштаб, `create_global`, `output_connected`. Таймер calloop на частоту режиму, який
   спить, якщо немає пошкоджень і запитів захоплення. Кадр: `compose_frame`, потім
   `render_screencopy`, `render_capture_frames`, `post_render` (виклики кадрів клієнтам).
   Видалення: `output_disconnected`, відкладене зняття глобалі (як `remove_output_global`).
2. udev.rs: віртуальні виходи в `render_if_needed` (окремий прохід без DRM), у списку
   `wlr-output-management` (`collect_output_state_from_surfaces`), без гами і DPMS.
3. winit.rs: те саме для вкладеного запуску (зручно перевіряти без TTY).
4. IPC: `Request::VirtualDisplay { Create { size, refresh, name, geometry }, Remove, List }`
   у protocol.rs, обробник у mod.rs, підкоманда в client.rs.
5. `docs/ipc.md`, за потреби `config.reference.toml` (потім `docs/config.md` генерується).
6. Тести в `src/tests/`: створити, захопити кадр через `ext-image-copy-capture`, видалити,
   перевірка витоків у `Drop` фікстури.

Приблизний розмір: від 700 до 1100 рядків, з них близько половини тести. Окремими PR:
спершу ядро і IPC, потім `wlr-output-management` і конфіг, потім headless бекенд (#202).

Тонкі місця (з `dev/docs/caveats.md` і коду):

- Блокування екрана: `session_lock` чекає кадр блокування на кожному виході. Віртуальний
  вихід мусить показувати екран блокування (інакше захоплення покаже стільницю).
- Членство вікон у виходах лише через `refresh_window_outputs`. У driftwm уже є «virtual
  placeholder outputs» (місця від’єднаних моніторів), назва легко сплутати.
- Відновлення сесії й правила `[[outputs]]` прив'язані до імені виходу, тож ім'я має бути
  стабільним.
- Позиція в загальній розкладці: чи може курсор перейти на віртуальний вихід.
- Ніколи не тримати `output_state(..)` у scrutinee `if let`/`match` (clippy і паніка).

Що автор, найімовірніше, захоче обговорити спершу (питання в коментар #180):

1. Справжній `wl_output` чи лише ділянка полотна. Пропозиція: справжній вихід, вид якого
   за замовчуванням закріплено на ділянці полотна (це і є #180). Тоді захоплення через
   наявне джерело виходу, портал бачить його як монітор, вікно можна розгорнути на ньому.
2. Інтерфейс: `create -g` від slurp (як він хоче) і варіант без геометрії, лише
   `--size 1920x1080 --refresh 90 --name`, для програм на кшталт Prostir.
3. Вид: закріплений чи рухомий, як у справжнього виходу (з `camera --output`).
4. Час життя: до `remove` чи до закриття IPC-з'єднання (щоб збій клієнта не лишав
   невидимий монітор).
5. Частота: власний таймер чи кадр на запит захоплення.
6. Чи робити разом з headless (#202) або спершу лише поруч із udev і winit.

## niri

| Що | Подробиці |
|---|---|
| Правила | [CONTRIBUTING.md](https://github.com/niri-wm/niri/blob/main/CONTRIBUTING.md): нова функція має відповідати задуму niri, бажано вже узгодженому в issue чи discussion; малі самодостатні коміти, кожен збирається; rebase, не merge; `cargo +nightly fmt --all`; документувати опції у вікі; «Allow edits from maintainers». [Шаблон PR](https://github.com/niri-wm/niri/blob/main/.github/pull_request_template.md) з галочкою «I did not use an LLM to write this pull request code or description.» Шаблони issue: bug_report, other; ідеї в Discussions. Чат Matrix #niri:matrix.org. CLA і DCO немає. |
| Стиль комітів | «область: Опис» або просто наказовий спосіб («solid_color: Set UnderlyingStorage…», «Upgrade Smithay…»). |
| Політика ШІ | Заборонено. Цитата: «You're not allowed to use LLMs to *create*. All pull request code (including tests), issue descriptions, comments, public reviews, etc. must be human-written. You can use LLMs privately (not like I can check or control that), but not to write from scratch. You may use LLMs for *translating* your human-written text; in this case you must also provide the original text.» |
| Стан | [PR #3800 «Add virtual output support»](https://github.com/niri-wm/niri/pull/3800) (willybarret, з 2026-04-09, +1792/-103, відкритий, оновлено 2026-11-08): headless бекенд і віртуальні виходи поруч із TTY, створення через IPC і config.kdl. Рецензії від YaLTeR (Ivan Molodetskikh, автор niri) немає; користувачі хвалять, є звіт про падіння при видаленні вимкненого виходу. Обговорення: [#3101](https://github.com/niri-wm/niri/discussions/3101) (YaLTeR: для віртуальних виходів «you want synthetic fixed refresh rate»), [#714](https://github.com/niri-wm/niri/discussions/714) (YaLTeR: «I don't think anything is missing on the Smithay side for headless outputs»). `ext-image-copy-capture` для виходів і курсора злито в [#4554](https://github.com/niri-wm/niri/pull/4554) (2026-09-12). |
| Хто і як активно | YaLTeR комітить щодня, близько 29 тис. зірок. Сам пише, що PR більше, ніж він встигає, і просить тестувати й рецензувати. |
| Перший крок | Власник збирає гілку PR #3800, перевіряє з Prostir (створення, захоплення через `ext-image-copy-capture`, видалення) і пише звіт про тест у PR своїми словами. Код від нас не пропонувати. |

## COSMIC (cosmic-comp)

| Що | Подробиці |
|---|---|
| Правила | Загальний гід Pop!_OS: [pop-os/pop CONTRIBUTING.md](https://github.com/pop-os/pop/blob/master/CONTRIBUTING.md). Для великих функцій спершу issue. Кожен PR схвалюють інженери і QA, для змін вигляду ще UX. [Шаблон PR](https://github.com/pop-os/cosmic-comp/blob/master/.github/PULL_REQUEST_TEMPLATE.md): без ШІ, автор розуміє зміни, опис у коміті, перевірено, DCO («I have read the Developer Certificate of Origin and certify my contribution»), без галочки PR закривають. Кодексу поведінки в репозиторії немає. |
| Стиль комітів | «область: Опис» («stack: Better/more correct way…», «grab: Remove internal mutexes…») або «feat: …». |
| Політика ШІ | Заборонено. Цитата: «Content generated by any LLM (also known as AI) may not be contributed in issues or PRs to Pop!_OS projects including code, comments, and descriptions.» |
| Стан | [#2878 «Software-only (virtual) outputs: prototype and questions before a PR»](https://github.com/pop-os/cosmic-comp/issues/2878) (AlexisCubilla, 2026-09-22): прототип на winit і KMS, свій маленький протокол, вихід прив'язаний до клієнта, захоплення як у справжнього; чотири питання до команди. Коментарів нуль за два місяці. Сам автор пише «The prototype was written with AI assistance», що суперечить правилам. Пов'язані: #1737 (headless), #1985 (віддалений дисплей), #584 (транспорт окремою програмою). `ext-image-copy-capture` у COSMIC є (кілька відкритих багів: #1405, #2383, #2954, #3101). Портал xdg-desktop-portal-cosmic VIRTUAL не оголошує. |
| Хто і як активно | System76, основний розробник композитора Ian Douglas Scott, коміти щотижня. |
| Перший крок | Чекати відповіді команди в #2878. Якщо власник хоче, короткий коментар своїми словами: випадок Prostir і відповідь на питання 3 (прив'язка до клієнта). |

## xdg-desktop-portal

| Що | Подробиці |
|---|---|
| Правила | Документи для учасників: [for-contributors](https://flatpak.github.io/xdg-desktop-portal/docs/for-contributors.html), [pull-requests.rst](https://github.com/flatpak/xdg-desktop-portal/blob/main/doc/pull-requests.rst), [merge-requirements.rst](https://github.com/flatpak/xdg-desktop-portal/blob/main/doc/merge-requirements.rst), [api-design.rst](https://github.com/flatpak/xdg-desktop-portal/blob/main/doc/api-design.rst), [experimental-portals.rst](https://github.com/flatpak/xdg-desktop-portal/blob/main/doc/experimental-portals.rst). Нові портали й функції спершу в [Discussions](https://github.com/flatpak/xdg-desktop-portal/discussions). Нетривіальні зміни API потребують ACK від представника кожного робочого столу (GNOME, KDE та інші). Можна почати з експериментального інтерфейсу `org.freedesktop.portal.experimental.{Name}{N}`. Порожні issue вимкнено, є шаблон bug-report, допомога в Matrix #xdg-desktop-portals:matrix.org. CLA і DCO немає. |
| Стиль комітів | gitlint: заголовок `префікс: опис`, за правилами GNOME, тег `Fixes:`; кожен коміт збирається, без merge-комітів. |
| Політика ШІ | Письмової політики в репозиторії й документації не знайшли. Перевірити ще раз перед внеском. |
| Стан | ScreenCast уже має тип джерела `4: VIRTUAL: Extend with new virtual monitor` ([PR #595](https://github.com/flatpak/xdg-desktop-portal/pull/595), 2021), але без опцій розміру, частоти й імені. Бекенди: GNOME так (Mutter `RecordVirtual`), KDE так (розмір обирають у діалозі), wlr ні (лише монітор і вікно), COSMIC ні. Обговорення [#1872 «ScreenCast: CreateVirtualMonitor and RemoveVirtualMonitor»](https://github.com/flatpak/xdg-desktop-portal/discussions/1872) (Chrome Remote Desktop): jadahl (GNOME) проти, бо портал для того, щоб користувач сам ділився екраном, а не для віддаленого доступу без людини; він пише окрему специфікацію віддаленого стола (gitlab.freedesktop.org/jadahl/xdg-specs MR 1). davidedmundson (KDE) за невеликі доповнення до наявного. Також [#1820](https://github.com/flatpak/xdg-desktop-portal/discussions/1820) і issue [#2077](https://github.com/flatpak/xdg-desktop-portal/issues/2077) про координати вказівника для VIRTUAL. |
| Хто і як активно | Сім комітерів (Jonas Ådahl, Sebastian Wick, David Redondo та інші), рецензія довга. |
| Перший крок | У #1872 власник описує випадок Prostir: людина поруч і сама погоджується, тож це саме «ділитися екраном». Просить необов'язкові опції для VIRTUAL: розмір, частота, ім'я. Для Prostir це шлях, який уже працює на GNOME і KDE. |

## Monado

| Що | Подробиці |
|---|---|
| Правила | GitLab freedesktop: https://gitlab.freedesktop.org/monado/monado, [CONTRIBUTING.md](https://gitlab.freedesktop.org/monado/monado/-/blob/main/CONTRIBUTING.md). Обов'язково: кодекс поведінки freedesktop.org (https://www.freedesktop.org/wiki/CodeOfConduct/) і DCO 1.1, у кожному коміті `Signed-off-by:`. Великі зміни: спершу RFC issue або чернетка MR, починати після явного схвалення супровідника. Дрібні фазовані MR, clang-format, без попереджень, фрагменти змін для Proclamation (`doc/changes/README.md`). Обговорення публічно в GitLab. |
| Стиль комітів | Префікс з шляху: «a/glue: Add C++ glue…», «t/constellation: Convert…», «ipc: Stub…». |
| Політика ШІ | Окремої політики немає. Цитата щодо DCO: «Note that only a human can legally certify the DCO.» |
| Стан | Нічого про віртуальні монітори (це не їхня область). Для майбутнього `prostir-vr`: `XRT_MAX_LAYERS` на Linux 128, на Android 32 (`src/xrt/include/xrt/xrt_limits.h`); [#483](https://gitlab.freedesktop.org/monado/monado/-/work_items/483) (закрито 2025-11-25) про те, що понад 16 шарів драйвер не гарантує. Свіжі: #642 (кольори в компоновнику на NVIDIA з оверлеєм), #644 (RFC про захоплення вигляду застосунку). |
| Хто і як активно | Collabora та інші, коміти щодня. |
| Перший крок | Зараз нічого. Повернутися, коли буде `prostir-vr`: баги і RFC з перевіреними даними. |

## WiVRn

| Що | Подробиці |
|---|---|
| Правила | [CONTRIBUTING.md](https://github.com/WiVRn/WiVRn/blob/master/CONTRIBUTING.md): збірка в docs/building.md, clang-format, великі PR дуже обережно, нічого, що порушує OpenXR чи Vulkan. Шаблони issue: bug_report, feature_request (опис, причина, пропонована реалізація), other. Discussions вимкнено. CLA і DCO немає. |
| Стиль комітів | Наказовий спосіб без префікса («Fix buffer overflow when creating listen socket»), номер PR у дужках. |
| Політика ШІ | Заборонено, крім дрібниць. Цитата: «The project is developed by humans, machine generated code will not be accepted unless it is for trivial changes.» [AGENTS.md](https://github.com/WiVRn/WiVRn/blob/master/AGENTS.md): «You are not allowed to submit pull requests.» і «You are not allowed to report issues, write comments or interact in any way with project issues.» |
| Стан | Відкритих задач про окремі шари для моніторів не знайшли. Шари складає Monado на ПК і кодує одним потоком (це вже відомо з research-competitors-2026-11.md). Активно: коміти щотижня (Patrick Nicolas, galister та інші), близько 1900 зірок. |
| Перший крок | Зараз нічого. Лише для перевірки `prostir-vr` на Quest; баги звітує власник. |

## Рекомендований порядок

1. driftwm. Коментар у #180 з питаннями задуму (текст готую я, власник перевіряє і
   публікує). Після згоди: перший PR лише ядро і IPC (`create`, `remove`, `list`) для udev
   і winit, з тестами. Потім `wlr-output-management` і конфіг. Потім headless (#202).
   Для Prostir одразу дає бекенд «IPC + `ext-image-copy-capture-v1`».
2. niri. Власник тестує PR #3800 з Prostir і пише звіт своїми словами. Код не пропонуємо.
3. xdg-desktop-portal. Власник пише в #1872 про випадок Prostir і опції для VIRTUAL.
4. COSMIC. Чекати відповіді в #2878; за бажання короткий коментар власника.
5. Monado і WiVRn. Після появи `prostir-vr`.

Перед кожним кроком ще раз перевірити CONTRIBUTING і політику ШІ: вони змінюються.
