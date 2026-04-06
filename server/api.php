<?php
header("Access-Control-Allow-Origin: *");
header("Access-Control-Allow-Methods: POST, GET, OPTIONS");
header("Access-Control-Allow-Headers: Content-Type");
header("Content-Type: application/json");

$db_file = 'hub_db.sqlite';
$upload_dir = 'uploads/';

if (!file_exists($upload_dir)) {
    mkdir($upload_dir, 0777, true);
}

// SQLite Bağlantısı ve Tablo Oluşturma
try {
    $db = new PDO("sqlite:$db_file");
    $db->setAttribute(PDO::ATTR_ERRMODE, PDO::ERRMODE_EXCEPTION);
    
    $db->exec("CREATE TABLE IF NOT EXISTS messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        hub_id TEXT,
        sender_name TEXT,
        message TEXT,
        file_name TEXT,
        file_url TEXT,
        status TEXT DEFAULT 'pending', -- pending, approved, revised
        admin_note TEXT,
        created_at DATETIME DEFAULT CURRENT_TIMESTAMP
    )");
} catch (PDOException $e) {
    die(json_encode(["error" => $e->getMessage()]));
}

$action = $_GET['action'] ?? '';

// 1. Yeni Mesaj / Dosya Gönderimi
if ($action == 'send') {
    $hub_id = $_POST['hub_id'] ?? '';
    $sender = $_POST['sender_name'] ?? '';
    $msg = $_POST['message'] ?? '';
    
    $file_name = "";
    $file_url = "";
    
    if (isset($_FILES['file'])) {
        $ext = pathinfo($_FILES['file']['name'], PATHINFO_EXTENSION);
        $new_name = uniqid() . "." . $ext;
        if (move_uploaded_file($_FILES['file']['tmp_name'], $upload_dir . $new_name)) {
            $file_name = $_FILES['file']['name'];
            // Dinamik URL Oluşumu (onayapp klasör yapısına uygun)
            $protocol = isset($_SERVER['HTTPS']) && $_SERVER['HTTPS'] === 'on' ? "https" : "http";
            $host = $_SERVER['HTTP_HOST'];
            $path = dirname($_SERVER['PHP_SELF']);
            $file_url = $protocol . "://" . $host . ($path == "/" ? "" : $path) . "/uploads/" . $new_name;
        }
    }
    
    $stmt = $db->prepare("INSERT INTO messages (hub_id, sender_name, message, file_name, file_url) VALUES (?, ?, ?, ?, ?)");
    $stmt->execute([$hub_id, $sender, $msg, $file_name, $file_url]);
    
    echo json_encode(["status" => "success", "id" => $db->lastInsertId()]);
}

// 2. Mesajları Listeleme (Polling)
elseif ($action == 'fetch') {
    $hub_id = $_GET['hub_id'] ?? '';
    $last_id = (int)($_GET['last_id'] ?? 0);
    
    $stmt = $db->prepare("SELECT * FROM messages WHERE hub_id = ? AND id > ? ORDER BY id ASC");
    $stmt->execute([$hub_id, $last_id]);
    $messages = $stmt->fetchAll(PDO::FETCH_ASSOC);
    
    echo json_encode($messages);
}

// 3. Durum Güncelleme (Onay/Revize)
elseif ($action == 'update_status') {
    $msg_id = $_POST['id'] ?? '';
    $status = $_POST['status'] ?? ''; // approved, revised
    $note = $_POST['note'] ?? '';
    
    $stmt = $db->prepare("UPDATE messages SET status = ?, admin_note = ? WHERE id = ?");
    $stmt->execute([$status, $note, $msg_id]);
    
    echo json_encode(["status" => "updated"]);
}
?>
