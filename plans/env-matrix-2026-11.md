# Матриця середовищ: що вдалося перевірити в контейнерах (2026-11-25)

Скрипти: `tests/env-matrix/` (README англійською). Гілка `env-matrix` від `dev/0.2.0`.
Усе в rootless podman, без sudo. Кожен контейнер має власні D-Bus, PipeWire, WirePlumber і
композитор, без мережі, без сокетів хоста. Prostir справжній (бекенд KDE або Hyprland),
кодувальник x264, шолом фальшивий (`prostir-probe` з `prostir-testkit`, ключі довірені заздалегідь).

## Головний висновок

Так, безголовий композитор у контейнері працює, і Prostir створює віртуальний монітор та
віддає закодовані кадри. Доведено для KWin 6.7.6 (Fedora 43) і KWin 6.8.3 (Arch): 188 і 231 кадр
H.264 за 14 секунд, перший кадр ключовий, потік парситься. Для CI є одна умова: KWin 6
у режимі `--virtual` вимагає вузол DRM (`/dev/dri/renderD*`), інакше малює через QPainter і
відмовляє в захопленні ("Unsupported compositing type"). Програмний GL (llvmpipe) при цьому
достатній, справжня відеокарта не потрібна, лише сам вузол.

## Таблиця

Позначки: ТАК = монітор створено, кадри прийшли. Частково = щось із цього є. НІ = не вийшло.

| Середовище | Версії | Результат |
|---|---|---|
| KWin, Fedora 43 | KWin 6.7.6, PipeWire 1.4.11, WirePlumber 0.5.14, GStreamer 1.26.11, glibc 2.42 | ТАК (з ярликом) |
| KWin, Arch | KWin 6.8.3, PipeWire 1.6.11, WirePlumber 0.5.19, GStreamer 1.28.9, glibc 2.44 | ТАК (ярлик не потрібен) |
| KWin, Ubuntu 25.10 | KWin 6.4.5, PipeWire 1.4.7, GStreamer 1.26.6, glibc 2.42 | Частково |
| KWin, Ubuntu 24.04 | KWin 5.27.11, PipeWire 1.0.5, WirePlumber 0.4.17, GStreamer 1.24.2, glibc 2.39 | НІ |
| KWin, Debian 12 | KWin 5.27.5, PipeWire 0.3.65, WirePlumber 0.4.13, GStreamer 1.22.0, glibc 2.36 | НІ |
| Hyprland, Arch | Hyprland 0.57.2, aquamarine 0.16.0 | НІ (немає пристрою KMS) |
| Hyprland, Ubuntu 25.10 | Hyprland 0.41.2 (на wlroots) | Частково |
| GNOME Mutter, Fedora 43 | mutter 49.8 | Працює безголово, Prostir не запускався (бекенда GNOME ще немає) |
| Sway, Debian 13 | sway 1.10.1, PipeWire 1.4.2, GStreamer 1.26.2, glibc 2.41 | Працює безголово, Prostir не запускався |

## KWin: правило ярлика (перевірено на справжніх KWin)

Фіксована збірка: Fedora 43, KWin 6.7.6, гілка `kwin-access` (0bf6c90) і гілка dev.

| Режим | Реєстр для Prostir | Результат |
|---|---|---|
| Без ярлика | `zkde_screencast_unstable_v1` немає | Відмова, нове повідомлення з `kde/access.rs` ("KWin does not give this copy of Prostir access ... no application shortcut names it ... KWin 6.7.6") |
| Ярлик зі збірки dev (без `X-KDE-Wayland-Interfaces`), `Exec` = шлях програми | Немає | Відмова. Одного збігу шляху мало, потрібен рядок з інтерфейсом |
| Ярлик з гілки `kwin-access` (`X-KDE-Wayland-Interfaces=zkde_screencast_unstable_v1`), `Exec` = шлях програми, `kbuildsycoca6` | Є | ТАК |
| `KWIN_WAYLAND_NO_PERMISSION_CHECKS=1`, без ярлика | Є | ТАК |
| Arch, KWin 6.8.3, без ярлика | Є | ТАК (з 6.8 перевірки ярлика немає) |

Для будь-якої іншої програми (не Prostir) `zkde_screencast` у реєстрі KWin 6.7.6 немає, а у 6.8.3 є.
У журналі KWin (`kwin_core.debug`) видно "authorized <шлях> zkde_screencast_unstable_v1".
Ще одна знахідка: на Ubuntu пакет `kwin-wayland` без `kwin-common` (там плагіни) не дає
`zkde_screencast` зовсім, і `kbuildsycoca6` без `applications.menu` не вносить ярлики в кеш.
У справжньому сеансі Plasma обидва на місці, у мінімальному контейнері їх треба додати.

## KWin старіший за 6.5: чому не вийшло

Режим `--virtual` створює віртуальний вихід на запит програми лише з Plasma 6.5
(`VirtualBackend::createVirtualOutput` є в Plasma/6.5, у 6.4 і 5.27 немає). Тож у 6.4.5 і 5.27
Prostir отримує інтерфейс (ярлик працює), але KWin відповідає "Could not find output".
Це межа тестового режиму, а не обов'язково справжнього сеансу з DRM.
Спроба вкладеного режиму (KWin у вікні іншого KWin):

- 6.4.5: віртуальний вихід створено, потік домовлено (paused -> streaming), потім KWin
  падає (`std::out_of_range`, `map::at`) приблизно через секунду. Кадрів немає.
- 5.27.11: внутрішній KWin не піднявся (не досліджено).

Отже Plasma 5.27 і 6.0 до 6.4 у контейнері не підтверджено. Для них потрібна віртуальна машина
або жива сесія. Окрема знахідка для 5.27: `kscreen-doctor` падає без `qtwayland5`.

## Hyprland

- Arch, 0.57.2: Hyprland не стартує, `CBackend::create() failed`. Aquamarine бере
  розподільник пам'яті лише від бекенда DRM (потрібен пристрій KMS, `/dev/dri/card*`).
  Лише вузол рендера (`renderD*`) не допомагає. Рішення для CI: модуль ядра `vkms`
  (потрібен root на хості) або, для тесту, лише старіші Hyprland на wlroots.
  Також Hyprland не стартує під root без `--i-am-really-stupid`.
- Ubuntu 25.10, 0.41.2 (ще на wlroots): стартує з `WLR_BACKENDS=headless`, у реєстрі є
  wlr-screencopy. Prostir створив `Prostir-1` через IPC (`output create headless`), стан
  Running, захоплення домовлено (shm, 3840x2160), кодувальник запущено. Кадрів за 14 секунд немає.
  Не досліджено: імовірно, захоплення не завершується (скейл 2.0 на HEADLESS-2) або немає пошкоджень.
- У Fedora 43 і Debian 12 пакета Hyprland немає. Debian 13 теж без нього. Ubuntu 25.10 має 0.41.2,
  Ubuntu 26.04 має 0.53.3 (не перевірено, на aquamarine, очікувано як Arch).

## GNOME Mutter (лише запис)

Fedora 43, mutter 49.8, `mutter --headless --wayland --no-x11 --virtual-monitor 1920x1080`:
стартує. У реєстрі немає ні `zwlr_screencopy`, ні `ext_image_copy_capture`, ні kde-протоколів.
`org.gnome.Mutter.ScreenCast`: `CreateSession`, `RecordVirtual` і `Start` виконуються,
вузол PipeWire створюється (47). Читання кадрів `pipewiresrc` у тесті не вдалося
("target not found", не досліджено). Тож шлях кроку 11b (RecordVirtual) принципово можливий.

## Sway (лише запис)

Debian 13, sway 1.10.1, `WLR_BACKENDS=headless`: стартує. Є `zwlr_screencopy_manager_v1`,
`zwlr_output_manager_v1`, `zwlr_virtual_pointer_manager_v1`. `ext_image_copy_capture` немає
(у 1.10 його ще немає). Бекенда Sway у Prostir поки немає.

## glibc і залежності бінарника

- Збірка на Debian 12 неможлива: крейт `gstreamer-video` з `v1_24` вимагає GStreamer >= 1.24,
  а в Debian 12 його 1.22.0. Найстаріший дистрибутив, де Prostir збирається, Ubuntu 24.04 (1.24.2).
- Бінарник, зібраний на Ubuntu 24.04 (glibc 2.39) і на Fedora 43 (2.42), запускається на
  Fedora 43, Arch, Ubuntu 25.10 і навіть на Debian 12 (glibc 2.36): завантажувач лише попереджає
  про слабку версію `GLIBC_2.39`. Жорсткі версії доходять до `GLIBC_2.35`. Тобто справжня
  підлога glibc нижча, ніж `glibc >= 2.39` у `generate-rpm`. Але на Debian 12 кодування через
  GStreamer 1.22 не перевірено (KWin 5.27 не дав виходу).
- На Fedora 43 `x264enc` є лише з RPM Fusion (`gstreamer1-plugins-ugly`). Без нього
  програмного кодування немає. `vah264enc` у контейнерах немає (немає пристрою VA).

## Що потрібно для CI

1. Вузол DRM для KWin 6: на раннері без GPU `sudo modprobe vgem` або `vkms` (на
   GitHub-раннерах можливо, потрібне перевірити). Без нього KWin 6 малює QPainter, захоплення відмовляє.
2. Для Hyprland 0.4x (aquamarine) потрібен справжній пристрій KMS, тобто `vkms`. Без нього лише
   старий Hyprland на wlroots, і той ще не віддає кадри.
3. Контейнери: Fedora 43 (KWin 6.7), Arch (KWin 6.8, Hyprland), Ubuntu 24.04 (збірка).
   Образи важкі (до 3.7 ГБ у Arch), кешувати.
4. Для KWin до 6.7 у тесті ставити ярлик з `X-KDE-Wayland-Interfaces` на шлях збірки і
   запускати `kbuildsycoca6`, і мати `applications.menu`.
5. Для KWin 6.0 до 6.4 і Plasma 5.27 контейнера з `--virtual` недостатньо (див. вище): віртуальна
   машина або жива сесія.

## Як відтворити

`tests/env-matrix/run.sh image|build|check`, див. `tests/env-matrix/README.md`.
Для KWin 6 потрібні `ENVMATRIX_RENDER_NODE=1` (передає вузол рендера хоста в контейнер) і
`EM_SOFTWARE_GL=1` (Mesa рендерить програмно, відеокарта хоста не використовується для малювання).
