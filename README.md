# SwappableVRAM

Windows'ta arka plan uygulamalarının soğuk sayfalarını boş GPU belleğinde tutarak oyuna daha fazla fiziksel RAM bırakmayı amaçlayan geliştirme projesi. Başarı ölçütü ayrılan VRAM miktarı değil; gerçek RAM baskısında oyun frametime p99 ve %1 düşük FPS sonucudur.

**Henüz tamamlanmış RAM genişletme yazılımı değildir.** Gerçek CUDA motoru ve arayüz çalışıyor. Test imzalı bypass filtresi izole WinPE'de yüklendi; dosya bütünlüğü ve pagefile bildirimi doğrulandı. Deneysel cache bağlantısı, gerçek uygulama pagefile okumaları ve oyun kazancı henüz doğrulanmadı. Mevcut arayüz uygulama sayfalarını VRAM'e taşımaz.

## Bileşenler

| Bileşen | Dil | Mevcut durum |
| --- | --- | --- |
| CUDA bellek havuzu ve 4 KiB aktarım | Rust | RTX 5060 üzerinde veri bütünlüğü doğrulandı |
| SSD asıl kopya + bırakılabilir VRAM önbelleği | Rust | Donanımda 2.048 cache hit ve VRAM bırakıldıktan sonra 2.048 disk okuması doğrulandı |
| Ayrı süreç CUDA broker iletişim laboratuvarı | Rust | Gerçek GPU, nesil kontrolü, servis çıkışı ve yeniden slot kullanımını reddetme testleri geçti |
| Kernel sayfa nesli ve istek sahipliği | `no_std` Rust | Heap/bekleme kullanmaz; range ve write-cluster kontrolleri dahil 16 test geçti |
| Windows storage üst filtre köprüsü | KMDF/C | İzole WinPE bypass/disk bütünlüğü/pagefile bildirimi geçti; deneysel cache/Verifier eksik |
| Windows sürücü broker istemcisi | Rust | Query yolu doğrulandı; disk yığınından bağımsız KMDF kontrol bağlantısı test ediliyor |
| WDDM bütçe ve bellek maliyeti | Rust | CUDA LUID eşleşmesi, bütçe eventi ve gerçek tahsis/bırakma ölçüldü; düşük bütçede admission kapalı |
| Windows arayüzü | C#/WPF | RAM/GPU/süreç izleme, CUDA gecikme testi ve JSON raporu |
| Arayüz ↔ interaktif Rust broker | C#/Rust | Yerel named pipe, PID/oturum/nonce doğrulaması, durum/başlat/durdur protokolü; varsayılan profil offload açmaz |

Kaynak temelimiz [nbd-vram](https://github.com/c0dejedi/nbd-vram). Linux NBD/swap bağlantısını Windows'a aynen taşımak mümkün olmadığı için Windows paging/storage bağlantısı ayrı geliştiriliyor. [RamShared](https://github.com/emersonbusson/ramshared) SSD asıl kopya ve bırakılabilir cache tasarımı için incelendi; onun Windows pagefile fonksiyonları veya imzalı sürücüsü hazır bir bağımlılık olarak kullanılmıyor.

## Geliştirme

Güncel gerçek bilgisayar ölçümleri ve yerel backend sınırı [yerel geliştirme durumunda](docs/LOCAL-DEVELOPMENT.md). `svram host-status`, runtime durumu salt okunur sorgular; GUI de bu bilgisayarda sürücü bağlantısı bulunup bulunmadığını gösterir.

Windows x64, Rust 1.97, MSVC x64 Build Tools, .NET 8 SDK ve CUDA destekli NVIDIA sürücüsü gerekir. CUDA Toolkit gerekmez. Sürücü derlemesi için proje içine çıkarılan eşleşen WDK/SDK 28000.2526 kullanılır; araçları hazırlayan betik sisteme sürücü veya WDK kurmaz.

```powershell
# Kullanıcı modu; yönetici izni gerekmez.
dotnet build SwappableVRAM.sln -c Release
dotnet run --project src/SwappableVRAM.Diagnostics -c Release -- test
cargo test --manifest-path native/Cargo.toml
cargo test --manifest-path drivers/paging-core/Cargo.toml

# Kaynak klasörü içinde gerçek GPU + geçici test dosyası testi.
cargo run --release --manifest-path native/Cargo.toml --bin svram -- cache-smoke artifacts

# Ayrı özel çocuk süreçte GPU/IPC; Windows paging sürücüsü kullanılmaz.
cargo run --release --manifest-path native/Cargo.toml --bin svram -- ipc-benchmark artifacts/measurements/ipc.json

# Bu bilgisayardaki GPU'da 4–64 KiB bütün aralık/cache testi; sürücü yüklemez.
cargo run --release --manifest-path native/Cargo.toml --bin svram -- range-benchmark artifacts/measurements/ranges.json

# Yerel Windows'ta gerçek overlapped I/O / iptal / read önceliği testi; pagefile değildir.
cargo run --release --manifest-path native/Cargo.toml --bin svram -- io-smoke

# Taşınabilir geliştirme arayüzü; derler, çalıştırmaz.
powershell -NoProfile -File scripts/Build-Desktop.ps1

# Aynı yerel Windows'ta arayüz kontrol protokolü; GPU tahsis etmez.
dotnet run --project src/SwappableVRAM.Diagnostics -c Release -- controller-test artifacts/desktop/SwappableVRAM.Broker.exe

# Yalnız derleme/paket hazırlığı. Hiçbir sürücü yüklemez.
powershell -NoProfile -File scripts/Prepare-DriverToolchain.ps1
powershell -NoProfile -File scripts/Build-Driver.ps1
powershell -NoProfile -File scripts/Prepare-Inf2Cat.ps1 -FetchVerifiedPackage
powershell -NoProfile -File scripts/Prepare-LabPackage.ps1 -ExperimentalCache -Catalog

# Yalnız izole lab için; host güven deposuna/boot ayarlarına dokunmaz.
pwsh -NoProfile -File scripts/Sign-LabPackage.ps1 -PackageDirectory artifacts/driver-package-experimental -TestOnly

# Salt okunur PnP envanteri; sürücü yüklemez veya etkinleştirmez.
cargo run --release --manifest-path native/Cargo.toml --bin svram -- driver-list
```

Derleme betiği mevcut VS18/MSVC 14.51.36231 konumunu kullanır. Başka makinede bu konum değiştirilmelidir. İndirilen Microsoft NuGet paketlerinin imzaları hazırlama betiğinde doğrulanır. Çıktılar `artifacts` altında tutulur, Git'e dahil edilmez.

## Gerçek cihazda ölçülenler

4–64 KiB yerel CUDA range testleri ve düzeltilen sıfırlama yarışı [range cache belgesinde](docs/RANGE-CACHE.md). Bu sonuçlar kernel paging veya oyun ölçümü değildir.

2026-11-03, Windows 11 build 28000, RTX 5060/WDDM, 4 KiB QD1:

| Deney | p50 | p99 | Kanıt sınırı |
| --- | ---: | ---: | --- |
| İlk C# CUDA prototipinde okuma | 52,8 µs | 109,0 µs | Süreç içi aktarım; Windows page fault değildir |
| Aynı deneyde C: önbelleksiz dosya okuma | 109,7 µs | 416,8 µs | Disk denetleyici cache'i atlanmaz; bütün diskler için genellenemez |
| Ayrı süreç Rust CUDA mailbox okuması | 68,5 µs | 154,3 µs | 2.000 örnek, 1 MiB corpus, 16 MiB VRAM; KMDF IOCTL yolu değildir |
| Gerçek timeout ve geç cevap testleri eklenen ayrı süreç deneyi | 69,5 µs | 156,8 µs | Timeout ve geç cevaptan sonra aynı slotun kullanımı reddedildi; kernel testi değildir |

Ham örnekler `artifacts/measurements` içindedir. Farklı deneylerin tek ölçümünü hızlanma oranı veya FPS kazanımı olarak yorumlamayın. GPU yükü, WDDM geri alma, gerçek paging yolu ve oyun A/B testi henüz ölçülmedi.

## Tamamlanması gereken asıl iş

1. Yerel Windows hedefinde güncel filtre bağlantısını, disk kimliğini ve gerçek private uygulama pagefile okumalarını doğrulamak.
2. Deneysel broker yolunda cache hit/miss, write coherence, timeout/cancel/teardown, düşük bellek forward progress ve Driver Verifier kampanyasını tamamlamak.
3. Kullanıcının seçtiği yerel geliştirme yolunda yüklenebilir test paketi, dar disk hedefi ve kurulum/geri alma yolunu tamamlamak. Microsoft/WHQL/attestation imzası bu projenin teslim şartı değildir; Windows'un yükleme politikası yine geçerlidir.
4. WDDM bütçesini, GPU baskısında VRAM bırakmayı ve arka plan süreçleri/oyun RAM önceliğini gerçek sürücüyle bağlamak.
5. Aynı CS2 + Steam + ChatGPT + Claude + Lightshot + OBS + Discord yükünde kontrol/aktif A/B testi yapmak: net RAM kazanımı, hard-fault gecikmesi, frametime p99 ve %1 düşük FPS.

Üretim sisteminin test-signing, Secure Boot, HAGS, pagefile veya sürücü ayarları değiştirilmedi. Windows sürücüsünün derlenmesi veya INF'nin geçmesi, bu adımların tamamlandığı anlamına gelmez. Ayrıntılar [sürücü durumunda](docs/DRIVER-STATUS.md).

Kullanıcı 2026-11-05'te güvenilir sürücü imzası hedefini kaldırdı. Seçilen yol kişisel geliştirme/test paketi olarak devam eder. Bu, stok Windows x64'ün imzasız kernel kodu yüklemesine izin verdiği anlamına gelmez: [yerel test imzası sınırı](docs/LOCAL-DEVELOPMENT.md). Yeni VM çalışması yapılmaz; arayüz/broker/GPU geliştirmesi bu bilgisayardadır.
