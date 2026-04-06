<?php
$db_file = 'hub_db.sqlite';
$upload_dir = 'uploads/';
$days_to_keep = 15;

try {
    $db = new PDO("sqlite:$db_file");
    
    // DB'den eski kayıtları seç
    $stmt = $db->prepare("SELECT file_url FROM messages WHERE created_at < datetime('now', '-$days_to_keep days')");
    $stmt->execute();
    $old_files = $stmt->fetchAll(PDO::FETCH_ASSOC);
    
    // Fiziksel dosyaları sil
    foreach ($old_files as $row) {
        if (!empty($row['file_url'])) {
            $path = $upload_dir . basename($row['file_url']);
            if (file_exists($path)) @unlink($path);
        }
    }
    
    // DB kayıtlarını sil
    $db->exec("DELETE FROM messages WHERE created_at < datetime('now', '-$days_to_keep days')");
    
    echo "Temizlik tamamlandı. $days_to_keep günden eski veriler ve dosyalar silindi.";
} catch (PDOException $e) {
    echo "Hata: " . $e->getMessage();
}
?>
