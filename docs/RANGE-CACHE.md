# Büyük paging okumalarını tek GPU aktarımıyla karşılamak

4 KiB'lik bir cache, Windows'un daha büyük paging okumalarını hızlandıramaz. Güncel kaynak 4 KiB hizalı 4–64 KiB okumalar için her sayfanın generation'ını tutar. Bütün aralık eşleşirse hit verir; tek bir eksik/eski sayfa tüm okumayı SSD'ye bırakır. Parçalı bir hit ile disk verisi aynı istekte birleştirilmez.

Rust cache aralığı ardışık VRAM slotlarına yerleştirir. Ardışık 16 sayfa için 16 ayrı CUDA kopyası/senkronizasyonu yerine bir 64 KiB aktarımı yapılır. Dört kopyadan fazla gerektiren parçalanmış yerleşimler gecikmeyi biriktirmeden miss döner. Bütün anahtarlar GPU aktarımından önce kontrol edilir. Bir payload/checksum veya GPU hatası cache'i ve CPU indeksini bırakır.

## Yerel gerçek GPU sonucu

Ham kanıt: `artifacts/measurements/local-ranges-init-fixed-20261104.json`. RTX 5060, 16 MiB VRAM havuzu; her boyut için 512 tam veri bütünlüğü kontrolü ve tam olarak 512 CUDA okuma çağrısı:

| Aralık | p50 | p99 |
| --- | ---: | ---: |
| 4 KiB | 50,7 µs | 116,7 µs |
| 8 KiB | 53,2 µs | 120,3 µs |
| 16 KiB | 61,0 µs | 151,5 µs |
| 32 KiB | 78,7 µs | 176,9 µs |
| 64 KiB | 126,0 µs | 197,6 µs |

Bu süreler aynı süreçteki Rust cache/GPU/checksum yolunu kapsar; KMDF IOCTL, gerçek Windows page-fault gecikmesi veya FPS değildir. Tek sayfalık stale-generation denemesi, GPU çağrısı yapılmadan ve çıktı buffer'ı değiştirilmeden miss verdi. Cache bırakma kontrolü de geçti.

## Başlangıç yarışının düzeltilmesi

İlk gerçek range testi, ilk GPU slotunda checksum hatası verdi; RAM birim testleri bunu yakalayamazdı. Eski başlangıç `cuMemsetD8` ile default stream'de sıfırlama yapıyor, veri aktarımını `CU_STREAM_NON_BLOCKING` başka stream'de yürütüyordu. Başka stream'in senkronizasyonu sıfırlamanın bittiğini garanti etmez.

Güncel kod `cuMemsetD8Async` ve aktarımları aynı sahipli stream üzerinde sıralar; oluşturma işlemi stream senkronizasyonu bitmeden havuzu dışarı vermez. Düzeltme sonrası bütün yerel range testleri ve .NET üzerinden sıfır başlangıç/bütünlük/çoklu thread/4 MiB'den büyük aktarım/dispose testleri geçti. NVIDIA'nın [senkronizasyon davranışı](https://docs.nvidia.com/cuda/cuda-driver-api/api-sync-behavior.html) ve [stream kuralları](https://docs.nvidia.com/cuda/cuda-driver-api/cuda_driver_api/group__CUDA__STREAM.html) bu sıralama gerekliliğini açıklar.

## Windows bağlantısının sınırı

Kernel kaynak protokolü v3'dir. READ ve MISS 192 bayt header taşır; FILL ve HIT yalnız gerçek payload'ı ekler. Query ABI v4/128 bayt, protokolü ve maksimum boyutu bildirir. Eski paket yeni istemcide kabul edilmez. Kernel stack'inde 64 KiB frame ayrılmaz. İlk MDL aralığı kapsamıyorsa normal disk yolu kullanılır.

Büyük yazmalarda tek write ticket tüm tamamlanma aralığını doğrular; daha sonra her fill dilimi tamamlanan yazmanın generation'ıyla tekrar eşleşmelidir. En fazla 16 dilim denenir. Hazır GET bulunmaması fill'i düşürür, disk yazmasını bekletmez. [16 GET'lik broker penceresi](BROKER-PIPELINE.md) uygulanıp yerel Win32 I/O ömrü testinden geçti; güncel kernel yolu ve gerçek paging hit oranı hâlâ doğrulanmalıdır.

Yeni range sürümü varsayılan ve deneysel KMDF derlemelerinde statik analizden geçti. Önceki 4 KiB sürümünün WinPE sonuçları yeni range kodu için kernel doğrulaması değildir. Yerel bilgisayara yeni sürücü yüklenmedi; RAM offload, Driver Verifier ve oyun A/B doğrulaması tamamlanmadı.
