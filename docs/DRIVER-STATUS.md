# Windows paging bağlantısı: kaynak prototipi

## Seçilen mimari

```text
Windows memory manager
        │ mevcut pagefile I/O
        ▼
KMDF storage üst filtresi ─── miss / broker yok / süre doldu ───► özgün disk yığını
        │                                                       ▲
        │ isteğe bağlı 4–64 KiB read                             │ tüm yazmalar
        ▼                                                       │
Rust CUDA broker ─── generation + payload doğrulanan hit ────────┘
        │
        ▼
GPU VRAM (yalnızca temiz, bırakılabilir kopya)
```

Çizimdeki hit doğrudan orijinal okuma isteğini tamamlar; yazmalar daima özgün diske gider. CUDA broker süreci asıl veri deposu değildir. Broker kapandığında veya GPU kopyası geciktiğinde sürücünün kendi kernel zamanlayıcısıyla okumayı diske iletmesi amaçlanır. Bypass sürücüsünün izole WinPE disk yığınına bağlanması ve canlı query yanıtı doğrulandı; deneysel cache ve gerçek masaüstü Windows paging davranışı henüz kanıtlanmadı.

`native/src/transport_lab.rs` Win32 mapping/event deneyidir. `native/src/driver_broker.rs` ise gerçek KMDF IOCTL istemcisidir. Mailbox ölçümü KMDF IOCTL ölçümü yerine geçmez.

## Kaynaktaki kurallar

- Normal sürücü derlemesinde cache etkinleştirme IOCTL'si `STATUS_NOT_SUPPORTED` döner. Deneysel derleme ayrıca seçilir.
- Storage read/write dispatch kodunda dispatcher wait veya CUDA API çağrısı yoktur. Alt diske `WdfRequestSend` asenkron gönderilir.
- 16 adet önceden ayrılmış pending slot vardır. Hazır bir broker GET isteği veya slot yoksa okuma hemen diske gider.
- Deneysel okuma deadline'ı 1 ms'dir. Bu değer kesin bir OS tamamlanma üst sınırı değildir: DPC/zamanlayıcı gecikmesi gerçek cihazda ölçülmelidir. Geç gelen hit kabul edilmez.
- Bekleyen okumalar `WdfRequestMarkCancelableEx` ile cancel callback'e kaydedilir. Timer/yanıt/kapatma yolu `WdfRequestUnmarkCancelable` başarılı olmadan isteği alamaz. `STATUS_CANCELLED`, isteği callback'e bırakır; timer aynı slotu tekrar kullanmaz. Kaydı baştan iptal edilmiş istek yalnız bir kez `STATUS_CANCELLED` ile tamamlanır. Bu kaynak protokolü derleme/statik analizden geçti; cancellation ve çıkarma yarışlarının Driver Verifier altında doğrulanması hâlâ açık bir engeldir.
- Güncel query ABI v4 (128 bayt), veri protokolü v3 ve maksimum aktarım boyutunu da döndürür. Kontrol oturumunu açan broker bu alanları doğrular; eski 4 KiB paketi yeni broker tarafından reddedilir. Aşağıdaki tarihsel canlı testler ABI v2/v3'tür; güncel range sürümünün canlı kernel başarısı olarak kullanılamaz.
- CUDA'ya verilen belleğin tek fiziksel/yerleşik VRAM olduğu garantisi yoktur. WDDM tahsisleri geri alabilir; host commit/backing maliyeti net RAM kazancından çıkarılmalıdır.
- `WdfIoForwardProgressReservedPolicyPagingIO` ile 32 WDF request/context rezervi istenir. Bu, tüm alt disk yığınının forward progress sağladığını veya OOM güvenliğini kanıtlamaz.
- 4 KiB hizalı, 4–64 KiB arası ve tam sayfa sayısına sahip paging okumaları adaydır. Her sayfanın generation'ı ayrı doğrulanır; tek sayfalık eksik/eski cache kaydı bütün isteği diske bırakır. İlk MDL tüm aralığı kapsamıyorsa (zincir dahil) kopyalama yapılmaz ve disk yolu kullanılır. Daha büyük okumalar ve normal disk I/O alt yığına gider.
- Büyük paging yazmalarında write ticket bütün aralığı ve global floor'u kontrol eder. Tamamlanan yazmanın generation'ı ile her fill dilimi tekrar eşleşmelidir. En fazla 16 adet 64 KiB dilim denenir; hazır broker GET yoksa kalan isteğe bağlı fill'ler beklemeden bırakılır. Bütün yazma payload'ının VRAM'e alındığı garantisi yoktur.
- Range protokolünde READ ve MISS yalnız 192 bayt header taşır; FILL/HIT header + gerçek payload taşır. Maksimum buffer 65.728 bayttır ve kernel stack'inde ayrılmaz. Değişken paket boyu ve bütün range alanları kopyadan önce doğrulanır.
- Fill payload kopyası device spinlock dışında yapılır. Epoch ve bütün generation'lar kopyadan önce ve sonra kilit altında kontrol edilir; arada bir yazma varsa fill düşürülür. Böylece 64 KiB kopya bütün disk isteklerinin kullandığı kilidi tutmaz.
- Bütün disk yazmaları, paging bayrağından bağımsız olarak ilgili hash bucket'larını geçersiz kılar. Yazma bitene kadar aynı bucket için hit verilmez. Çakışmalar doğru veriyi kaybetmeden yalnız hit oranını azaltır.
- TRIM/UNMAP veya passthrough SCSI gibi salt `IRP_MJ_WRITE` dışında veri değiştirebilen kontroller de kapsanır: alt yığına iletilen bütün storage IOCTL/flush istekleri boyunca global coherence barrier tutulur. Bu muhafazakâr davranış hit oranını azaltabilir; salt okunur kontrol allowlist'i henüz nitelendirilmedi.
- SSD'ye yazılan payload ancak tekil write ticket hâlâ geçerliyse VRAM'e doldurulabilir. Tamamlanma sırası farklı örtüşen yazmalar, eski payload'ı yeni generation etiketiyle dolduramaz.
- Read hit, sequence/volume epoch/page index/generation ve deadline eşleşmesiyle tek kez sahiplenilir. Yanıt ve timer aynı pending isteği iki kez tamamlayamaz; bu durum makinesi Rust birim testlerindedir.
- Broker sahibinin değişmesi, eski GET isteklerini yeni oturuma taşımaz. Owner epoch ve monoton sequence kullanılır.
- Güncel broker yetkisi PID ve ayrı KMDF control device üzerinde açılan dosyanın bağımsız, referans sayımlı oturum token'ına bağlıdır. Disk PnP arayüzü yalnız keşif/query içindir; OwnerInfo v3/32 bayt, boot boyunca tekil hedef ID'sini döndürür. Rust istemci bu ID'den kontrol adını oluşturur ve açılan uçta aynı ID'yi yeniden doğrular. Ham disk adı kabul edilmez. FileCleanup önce closing gate'ini kapatır; ENABLE aynı gate'i parent kilidi altında kontrol eder. Geç istek cleanup'ı yalnız bağımsız token referansı bırakır. Kontrol ve broker kuyrukları aynı CDO'ya aittir; aygıtlar arasında geçersiz queue forwarding yoktur.
- Raw PDO deneyi WinPE'de child devnode üretmedi. `partmgr` üstteyken BusRelations iletiminin kesilmesi olası neden olarak araştırıldı; bir PnP IRP izi olmadan kesin neden sayılmaz. Bağımsız control device yolu Microsoft Toaster SideBand örneğindeki belgeli alternatiftir; eski query başarısı yeni broker yolunun doğrulandığı anlamına gelmez.
- D0 giriş/çıkışında cache kapatılır ve bütün generation'lar geçersiz kılınır; otomatik GPU yeniden etkinleştirmesi yoktur. D0 dışında cache açılamaz. Güç/uyku/hotplug davranışı yine canlı test gerektirir.
- Self-managed suspend/flush kaynaktaki cache gate'ini kapatır, timer'ı durdurur, bekleyen cache okumalarını ayrı fallback kuyruğuna/iptal sahibine bırakır ve cache CPU lease'lerinin bitmesini bekler. Bu bekleme alt disk tamamlanması veya GPU/broker cevabı için değildir. Kaynak/statik analiz geçti; canlı D0/remove/cancellation ve alt yığının power güvenliği Driver Verifier ile hâlâ doğrulanmalıdır.
- Deadline saati `KeQueryInterruptTimePrecise` kullanır. Kaba interrupt clock 1 ms hit son tarihi için yeterli değildi. Kesin OS tamamlanma üst sınırı iddiası yoktur.
- Cache kaynakları ve sayfa buffer'ları kernel stack'ine büyük geçici nesneler olarak konulmaz. Rust paging çekirdeği `no_std` ve allocation-free'dir.

## Paket

Sürücü ABI hedefi KMDF 1.31 / Windows 10 2004 (19041) ve sonrasıdır. İzole WinPE bypass kanıtı vardır; masaüstü Windows 10/11 paging nitelendirmesi değildir. WDK/SDK 28000.2526 geliştirme araçları ile derlenir.

`svramflt.inf.in`, `Class=Extension`, `AddFilter` ve `FilterPosition=Upper` kullanır. `disk.sys` işlev sürücüsünü değiştiren associated service tanımlamaz; DiskDrive sınıfı `UpperFilters` kaydını değiştirmez.

Varsayılan HWID `SVRAMLAB\DisposableDisk` yalnızca şablon doğrulaması içindir ve normal diske eşleşmez. Paket betiği genel `GenDisk`, Msft/QEMU ve fiziksel disk kimliklerini kabul etmez. Gerçek izole test diski için özel `SVRAMLAB` vendor/HWID gerekir.

INF hardware ID ile eşleşir; instance ID ile eşleşmez. Aynı HWID'yi taşıyan tüm cihazlar eşleşebilir. İzole VM'de o HWID'li tek test diski bulunması ayrıca kanıtlanmalıdır. Hazır paket oluşturmak, kurulum yetkisi veya cihaz kimliği kanıtı değildir.

## Doğrulama durumu

| Gereksinim | Kanıt | Durum |
| --- | --- | --- |
| Gerçek CUDA VRAM aktarımı | Donanım bütünlük testleri | Geçti |
| SSD asıl kopya ve cache bırakma | 2.048 VRAM hit + bırakma sonrası 2.048 disk okuması | Geçti; kullanıcı modu block API |
| Broker süreç sınırı | Gerçek CUDA ile mapping/event testi ve ham süreler | Geçti; kernel IOCTL değildir |
| Generation/overlap/request ownership | 13 `no_std` core birim testi | Geçti; kernel concurrency kanıtı değildir |
| KMDF + Rust link | Varsayılan ve deneysel x64 SYS çıktıları | Geçti |
| C köprüsü statik analiz | MSVC `/analyze`, `/W4 /WX`; Rust panic fail-fast çağrısında açıklanmış tek C28159 istisnası | Geçti; Driver Verifier veya canlı test yerine geçmez |
| Extension INF | Microsoft InfVerif `/w /v` | Geçti; sentetik HWID |
| İmzasız katalog | Resmi Inf2Cat signability testinde hata/uyarı yok, CAT üretildi | Geçti; kurulum veya üretim güven zinciri değildir |
| İzole laboratuvar imzası | Ayrı bypass/deneysel paketlerin SYS ve CAT dosyaları geçici sertifikayla imzalandı; katalog kriptografisi doğrulandı | Geçti; üretim güveni veya yükleme kanıtı değildir |
| Windows cihazına bağlanma | WinPE PnP stack: partmgr/svramflt/Disk/LSI_SAS; tek private arayüz, canlı ABI v2/mode 0, iki iletilmiş okuma | Geçti: yalnız izole WinPE bypass; masaüstü Windows ve loaded-SYS hash karşılaştırması eksik |
| Windows paging-file bildirimi | WinPE'de R: test diskinde Wpeutil CreatePageFile başarılı; aynı filtre query'sinde PagingPaths 0→1 | Geçti: izole WinPE bypass; gerçek masaüstü Windows ve anonymous-page okuma testi eksik |
| KMDF IOCTL + gerçek CUDA | Cache hit/miss/integrity sayaçları | Eksik |
| 4–64 KiB range kaynak yolu | 16 no_std test, variable wire ABI testleri, iki sürücü derlemesinde MSVC analiz | Geçti: kaynak/derleme; güncel range kodu host kernel'de yüklenmedi |
| 16 GET / read önceliği / kapanış ömrü | 4–64 KiB gerçek yerel named-pipe paketleri, pending iptal/drop, 32 yarış, büyük paketi reddetme | Geçti: Win32 I/O; KMDF veya pagefile kanıtı değil |
| Eşzamanlı kontrol çağrısı | Aynı bağlantıda dört thread / 256 salt okunur yerel dosya kontrolü | Geçti: event serileştirme ve tamamlanan byte sayısı; kernel backend değil |
| Güncel CDO kontrol ucu | OwnerInfo v3: kendi WDF file/session'ı, doğru target ID, query v3/120 bayt | Geçti: izole WinPE, RAM fixture |
| Kernel geç cevap ve miss | 20 ms gecikmede 2 deadline fallback + 2 geç yanıt; forced miss'te 3 reply fallback; bütünlük korundu | Geçti: RAM fixture; GPU veya anonymous-page hit kanıtı değil |
| Broker süreç ölümü ve handle ayrımı | İlk okumada exit 87; disk bütünlüğü ve son mode 1/lease 0; aynı PID'nin farklı handle'ı ENABLE/DISABLE/GET/REPLY için reddedildi | Geçti: izole WinPE, RAM fixture; bütün remove/cancel yarışları değildir |
| Düşük RAM, iptal, servis/GPU kaybı, kaldırma | Verifier ve hata enjeksiyonu | Eksik |
| Microsoft üretim imzası | WHQL/attestation | Kullanıcının 2026-11-05 kararıyla hedef kapsamından çıkarıldı; Windows yükleme politikası sürer |
| Yerel geliştirme kurulumu/geri alma | Test paketi + dar disk hedefi + rollback | Eksik; host ayarları değiştirilmedi |
| Oyun kazancı | Aynı yükte A/B frametime + %1 low + net RAM | Eksik |

## Güncel kontrol ve hata kanıtları

- `artifacts/vm-lab/inventory-20261104-025314-9133/results/GUEST.LOG`: CDO bağlantısı, 218 fill, 2 issued read, 2 deadline fallback ve 2 geç yanıt. 20 ms broker gecikmesine rağmen dosya bütünlüğü ve pagefile bildirimi geçti; çıkış mode 1, lease 0.
- `artifacts/vm-lab/inventory-20261104-025528-0434/results/GUEST.LOG`: forced miss; 3 issued read, 3 reply fallback, 205 fill, hit 0; dosya bütünlüğü ve pagefile bildirimi geçti. Bu iki fixture'ın SYS SHA-256 değeri `1AF68286387D58D15643D41A753B972944B2144338182DBB5141BA049177A3B1`.
- `artifacts/vm-lab/inventory-20261104-025714-3384/results/GUEST.LOG`: broker ilk kernel okumasında exit 87; 1 issued read sonrası veri bütünlüğü korundu, mode 1 ve lease 0 oldu. Aynı PID'deki ayrı handle'ın ENABLE/DISABLE/GET/REPLY denemeleri reddedildi. Bu fixture ayrıca son target-reference kapanış düzenini içerir; SYS SHA-256 `D45EEEC7356119F6B1A354F6085131CDC21189D3CBB04F8EFB9A20703BA03BC7`.

Konuk driver-store'dan çıkarılan SYS dosyaları paketle aynı hash'i verdi. Bu, çalışan kernel belleğinin hash'i değildir. Fixture'lar yalnız 8 MiB RAM kullanır; sonuçlar GPU hit'i, uygulama sayfalarının VRAM'e taşınması veya oyun kazancı olarak sunulmaz. Driver Verifier, gerçek CUDA ile kernel yolu ve anonymous-page fault testi açık işlerdir.

## Microsoft kaynakları

- [Asenkron WDF istek gönderimi](https://learn.microsoft.com/en-us/windows-hardware/drivers/wdf/sending-i-o-requests-asynchronously)
- [Özel dosya/paging desteği](https://learn.microsoft.com/en-us/windows-hardware/drivers/wdf/supporting-special-files)
- [Düşük bellekte forward progress](https://learn.microsoft.com/en-us/windows-hardware/drivers/wdf/guaranteeing-forward-progress-of-i-o-operations)
- [Paging bildiriminin storage yığınında ele alınması](https://learn.microsoft.com/en-us/windows-hardware/drivers/storage/handling-pnp-paging-requests)
- [Extension INF](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/using-an-extension-inf-file)
- [AddFilter](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/inf-addfilter-directive)
- [WDDM backing store](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/sharing-backing-store-with-kmd)
- [MarkCancelableEx: önceden iptal edilmiş istekte inline callback yoktur](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdfrequest/nf-wdfrequest-wdfrequestmarkcancelableex)
- [UnmarkCancelable: STATUS_CANCELLED sahipliği cancel callback'indedir](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdfrequest/nf-wdfrequest-wdfrequestunmarkcancelable)
- [Microsoft SkeletonI2C: kilit altında MarkCancelableEx örneği](https://github.com/microsoft/Windows-driver-samples/blob/main/spb/SkeletonI2C/device.cpp)

Windows'un genel uygulama sayfalarını VRAM'e taşımak için basit bir kullanıcı modu API'si bulunmadığından bu kernel katmanı gerekir. Working set temizleme veya süreç bellek önceliği değiştirme işlemleri VRAM offload yerine geçmez.

28000 NuGet setinde olmayan resmi Inf2Cat, 26100.6584 WDK NuGet paketinin doğrulanmış Microsoft yayıncı imzalı içeriğinden çıkarıldı. `WindowsProtectedFiles.xml` dahil gerçek signability/katalog üretimi çalıştı. Bazı araç DLL'lerinin Microsoft iç build sertifikası hostta güvenilir kök değildir; bunlar için bütün NuGet yayıncı imzası doğrulandı. Hostun sertifika deposuna kök eklenmedi. `-Catalog` ile hazırlanan paketler CAT içerir; `Signed=false`, `Installed=false`, `PagingPathQualified=false` kalır.

`Sign-LabPackage.ps1 -PackageDirectory ... -TestOnly`, ayrı bir adımda yalnız dar `SVRAMLAB` hedefli paketi geçici test sertifikasıyla imzalar. Ana makinenin Root/TrustedPublisher deposuna sertifika eklemez; test-signing açmaz veya sürücü yüklemez. Özel anahtar ve parolası yalnız Git dışında tutulan `artifacts/lab-signing` içindedir. İmzalı paket manifestindeki `ProductionTrusted=false` ve `PagingPathQualified=false` değerleri korunur. `Prepare-LabPackage.ps1` yeniden çalıştırılırsa imzasız yeni paket oluşturulur; eski test imzası yeni çıktıyı kapsamaz.

Rust CLI `driver-list`, SetupAPI üzerinden yalnız özel filtre arayüzlerini ve bağlı PnP instance kimliğini okur. Kurulum yapmaz; 2026-11-04 yerel kontrolde sıfır mevcut arayüz bulundu. `Prepare-WinPeAssets.ps1`, Microsoft imzalı WinPE bootstrapper'ına bağlı MSI/CAB içeriklerini kurucu çalıştırmadan proje altına açmak içindir. WinPE ile olası bir repro testi gerçek GPU, masaüstü Windows, Driver Verifier veya oyun performansı doğrulaması yerine geçmez.

İzole WinPE envanter deneyi gerçek olarak açıldı: Rust token kontrolü `winpe=true, local_system=true`, konuk BCD ayarı ve ayrı SVRAMLAB/LSI_SAS test diski ile QEMU/storahci boot diski kayda geçti. İlk deneyde `{current}` BCD alias'ı WinPE'de geçersizdi; güncel harness konfigürasyon yerine native runtime code-integrity bitini kontrol eder. Harness ve doğrulama sınırları [lab belgesindedir](../tests/lab/README.md).

2026-11-04 bypass attach kanıtı: `artifacts/vm-lab/inventory-20261104-011109-9828/results/GUEST.LOG`. Runtime test-signing biti doğrulandı; test sertifikası yalnız konuğa eklendi. `pnputil`, paketi `oem0.inf` olarak yalnız SVRAMLAB hedefe kurdu; cihaz restart sonrası doğru stack ile `Started` döndü. Query `size=104, version=2, mode=0, reads=2, paging_paths=0` verdi. Bu oturumda henüz pagefile oluşturulmadı ve cache hit'i yoktu. İmzalı kaynak SYS SHA-256: `08F0F9FA53E864F4C896AE6BAF4FB29EFBDD925A377E6F2E8AFE644F137382C2`; loaded image dosyasının bağımsız hash doğrulaması sonraki kontroldür.

Storage/pagefile kanıtı: `artifacts/vm-lab/inventory-20261104-012125-5480/results/GUEST.LOG`. Guard yalnız 1 GiB SVRAMLAB diski seçti; DiskPart bu hedefi boot/pagefile/hibernation/crashdump diski olmayan disposable aygıt olarak gösterdi. R: NTFS volume extent'i aynı disk numarasına doğrulandı. 256 doğrudan 4 KiB yazma ve karışık sırada okuma bütünlük testi geçti; test dosyası kaldırıldı. Wpeutil 256 MiB pagefile oluşturduktan sonra query `paging_paths=1, reads=373, writes=611, paging_reads=30, candidates=12, cache_hits=0` verdi. `IRP_PAGING_IO` sayacı dosya sistemi paging I/O'sunu da kapsar; bu 30 okumayı private uygulama pagefile fault'u veya VRAM hit'i olarak yorumlamayın.
