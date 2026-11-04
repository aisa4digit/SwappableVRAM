# Sonraki native bütçe katmanı

DXGI katmanı `native/src/wddm.rs` içinde Rust ile uygulandı; `wddm` Cargo feature'ı isteğe bağlıdır. Araştırma Luna / orta efor ile yapıldı. WinPE storage CLI'sinin varsayılan derlemesi DXGI import etmez.

## Doğru adaptör ve bütçe

- CUDA ordinalını `cuDeviceGetLuid` ile 8 bayt LUID ve node mask'a çevir. DXGI `EnumAdapters1` / `GetDesc1.AdapterLuid` ile eşleştir. İsim/ordinal/PCI-string sırasını adaptör kimliği sayma.
- Node mask tek bit değilse ilk uygulama kabul etmesin; çoklu node desteği ayrıca doğrulanmalı.
- Aynı broker PID'sinde `IDXGIAdapter3::QueryVideoMemoryInfo(node, LOCAL)` sorgula. `Budget - CurrentUsage`, global boş VRAM'den farklı bir process-local sınırdır. Oyun headroom'u ayrıca korunmalı.
- `AvailableForReservation` boş VRAM değildir; `CurrentReservation` allocation/yerleşiklik garantisi değildir. Rezervasyon API'sini oyunla rekabet etmek için kullanma.
- Bütçe değişikliği event'i + timeout kullan; her çıkışta notification cookie'yi unregister et, sonra event handle'ını kapat. Query veya eşleştirme başarısızsa admission kapalı kalsın.
- CUDA tüketiminin DXGI process kullanımında izlenmesi bu RTX 5060 üzerinde gerçek allocate/free ile doğrulandı. Bu ölçüm diğer sürücü/GPU kombinasyonları için garanti değildir.

Microsoft `windows` / windows-rs 0.62.2 bağlayıcısı manuel COM vtable'dan daha uygun. İlgili feature'lar Dxgi, Foundation ve event API'leri için Threading/Security'dir. WinPE lab CLI'sinin yükleme bağımlılıkları korunmalı; optional feature veya ayrı küçük probe binary kullanılması düşünülebilir. WinPE'de DXGI varlığını varsayıp bütün storage test CLI'sine zorunlu import eklemeyin.

## Gerçek tahsis maliyeti

2026-11-04 ölçümleri: `artifacts/measurements/wddm-cost-{16,512}-20261104-021155.json`. 16 MiB tahsiste local kullanım 112 MiB, 512 MiB tahsiste 608 MiB oldu; iki deneyde non-local kullanım 78 MiB ve process working-set artışı yaklaşık 100 MiB idi. Private commit artışı sırasıyla yaklaşık 216 ve 713 MiB oldu. Local/non-local kullanım havuz bırakıldığında sıfıra döndü; bütçe eventi gözlendi. Başlangıç RAM örneği CUDA sürücü init'inden sonradır; tüm uygulama başlangıç maliyeti değildir. Global available RAM farkı başka süreçlerden de etkilenir.

Bu nedenle commit maliyeti ile fiziksel RAM maliyeti ayrı izlenir. Admission, bilinmeyen/düşük commit'i reddeder. Deneysel broker aynı PID'de bütçe/commit/non-local/working-set sınırlarını izler ve ihlalde cache'i kapatır. Ölçümler henüz uygulama sayfası offload'u, net RAM kazancı veya FPS kanıtı değildir.

## Açık ürün işi

Self-managed suspend/drain kaynak protokolü uygulandı; canlı Verifier ve deneysel kernel broker hit/miss/fallback/iptal testleri tamamlanmalı. Aktif pagefile bildirimi bu testlerin yerine geçmez. Daha büyük Windows paging I/O boyutları (şu an yalnız 4 KiB aday) ve batch GPU aktarımı gerçek izlerle değerlendirilmeli. Arka plan uygulama politikası ancak nitelendirilmiş backend'e bağlanmalı; working-set trim'i kendi başına VRAM offload sayılmaz.

Normal Windows dağıtımı için Microsoft güven zinciri hâlâ dış bağımlılıktır. Hazır WinSpd/ImDisk/AIM proxy sürücülerinde aynı paging + SSD-authoritative broker-death fallback davranışı için belgeli bir yerine-geçen yol bulunmadı. Geçici test sertifikası üretim imzası değildir. Kullanıcının host test-signing/Secure Boot ayarlarını değiştirmek bu engelin çözümü olarak kullanılmadı.

## Birincil kaynaklar

- [CUDA Driver API / cuDeviceGetLuid](https://docs.nvidia.com/cuda/cuda-driver-api/cuda_driver_api/group__CUDA__DEVICE.html)
- [QueryVideoMemoryInfo](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiadapter3-queryvideomemoryinfo)
- [DXGI bellek alanları](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/ns-dxgi1_4-dxgi_query_video_memory_info)
- [Bütçe değişikliği eventi](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiadapter3-registervideomemorybudgetchangenotificationevent)
- [Microsoft Rust bağlayıcıları](https://microsoft.github.io/windows-docs-rs/doc/windows/Win32/Graphics/Dxgi/struct.IDXGIAdapter3.html)
- [Microsoft driver signing seçenekleri](https://learn.microsoft.com/windows-hardware/drivers/dashboard/driver-signing-offerings)
- [Attestation gereksinimleri](https://learn.microsoft.com/windows-hardware/drivers/dashboard/code-signing-attestation)
