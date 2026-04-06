<?php
header("Access-Control-Allow-Origin: *");
header("Access-Control-Allow-Methods: POST, GET, OPTIONS");
header("Access-Control-Allow-Headers: Content-Type");
header("Content-Type: application/json");

// --- ŞİFRE AYARLARI ---
$hub_id = "grafik-tasarim"; // Sabit Hub ID
$member_password = "grafik123";  // EKİP ÜYELERİ İÇİN ŞİFRE
$admin_password = "admin123";      // YÖNETİCİ/LİDER İÇİN ŞİFRE
// ----------------------

$db_file = 'hub_db.sqlite';
$upload_dir = 'uploads/';

if (!file_exists($upload_dir)) {
    mkdir($upload_dir, 0777, true);
}

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
        status TEXT DEFAULT 'pending',
        admin_note TEXT,
        created_at DATETIME DEFAULT CURRENT_TIMESTAMP
    )");
} catch (PDOException $e) {
    die(json_encode(["error" => $e->getMessage()]));
}

$action = $_GET['action'] ?? '';

// 1. Giriş Doğrulama
if ($action == 'login') {
    $pass = $_POST['password'] ?? '';
    if ($pass === $admin_password) {
        echo json_encode(["status" => "success", "role" => "admin"]);
    } elseif ($pass === $member_password) {
        echo json_encode(["status" => "success", "role" => "member"]);
    } else {
        echo json_encode(["status" => "error", "message" => "Hatalı şifre!"]);
    }
}

// 2. Mesaj/Dosya Gönderimi
elseif ($action == 'send') {
    $sender = $_POST['sender_name'] ?? '';
    $msg = $_POST['message'] ?? '';
    
    $file_name = "";
    $file_url = "";
    
    if (isset($_FILES['file'])) {
        $ext = pathinfo($_FILES['file']['name'], PATHINFO_EXTENSION);
        $new_name = uniqid() . "." . $ext;
        if (move_uploaded_file($_FILES['file']['tmp_name'], $upload_dir . $new_name)) {
            $file_name = $_FILES['file']['name'];
            $protocol = isset($_SERVER['HTTPS']) && $_SERVER['HTTPS'] === 'on' ? "https" : "http";
            $host = $_SERVER['HTTP_HOST'];
            $path = dirname($_SERVER['PHP_SELF']);
            $file_url = $protocol . "://" . $host . ($path == "/" ? "" : $path) . "/uploads/" . $new_name;
        }
    }
    
    $stmt = $db->prepare("INSERT INTO messages (hub_id, sender_name, message, file_name, file_url) VALUES (?, ?, ?, ?, ?)");
    $stmt->execute([$hub_id, $sender, $msg, $file_name, $file_url]);
    echo json_encode(["status" => "success"]);
}

// 3. Mesajları Çekme
elseif ($action == 'fetch') {
    $last_id = (int)($_GET['last_id'] ?? 0);
    $stmt = $db->prepare("SELECT * FROM messages WHERE hub_id = ? AND id > ? ORDER BY id ASC");
    $stmt->execute([$hub_id, $last_id]);
    echo json_encode($stmt->fetchAll(PDO::FETCH_ASSOC));
}

// 4. Onay/Revize
elseif ($action == 'update_status') {
    $msg_id = $_POST['id'] ?? '';
    $status = $_POST['status'] ?? '';
    $note = $_POST['note'] ?? '';
    $stmt = $db->prepare("UPDATE messages SET status = ?, admin_note = ? WHERE id = ?");
    $stmt->execute([$status, $note, $msg_id]);
    echo json_encode(["status" => "updated"]);
}
?>
