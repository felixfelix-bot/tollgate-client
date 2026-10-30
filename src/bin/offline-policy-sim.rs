//! Explicitly offline: no network client and no real delivery implementation.
#[cfg(any(target_os = "linux", target_os = "android"))]
mod simulation {
    use cdk::secp256k1::hashes::{sha256, Hash};
    use std::path::Path;
    use tollgate_client_core::{
        offline_policy::{Enrollment, PolicyAdapter},
        send_state::DeliveryObservation,
        token_vault::{ProtectedTokenVault, VaultRef},
        validated_send::VerifiedBytesTransport,
    };
    struct Simulation;
    impl VerifiedBytesTransport for Simulation {
        fn send(&mut self, body: &[u8]) -> DeliveryObservation {
            println!("SIMULATION_CALLBACK {}", sha256::Hash::hash(body));
            DeliveryObservation::Unknown
        }
    }
    fn enrollment(path: &str) -> Result<Enrollment, String> {
        let bytes = std::fs::read(path).map_err(|_| "enrollment read failed")?;
        serde_json::from_slice(&bytes).map_err(|_| "invalid enrollment JSON".into())
    }
    fn number(s: &str) -> Result<u64, String> {
        s.parse().map_err(|_| "invalid number".into())
    }
    pub fn run() -> Result<(), String> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.first().map(String::as_str) {
            Some("enroll") if args.len()==3 => {
                PolicyAdapter::enroll(Path::new(&args[1]), &enrollment(&args[2])?)?;
                println!("SIMULATION_ENROLLED");
            }
            Some("attempt") if args.len()==9 || args.len()==10 => {
                // Optional advertised amount is deliberately excluded from policy,
                // reservation and transport: only validated token face is charged.
                if args.len()==10 {let _advertised_amount=number(&args[9])?;}
                let expected=enrollment(&args[2])?;
                let snapshot=std::fs::read(&args[3]).map_err(|_| "snapshot read failed")?;
                let vault=ProtectedTokenVault::open(Path::new(&args[4])).map_err(|_| "vault open failed")?;
                let digest=&args[5];
                if digest.len()!=64 || !digest.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {return Err("invalid vault digest".into());}
                let mut bytes=[0u8;32];
                for (i,b) in bytes.iter_mut().enumerate() {*b=u8::from_str_radix(&digest[i*2..i*2+2],16).map_err(|_| "invalid vault digest")?;}
                let usage=PolicyAdapter::open(Path::new(&args[1]))?.attempt(&expected,&snapshot,&vault,&VaultRef::from_digest(bytes),&args[6],number(&args[7])?,number(&args[8])?,&mut Simulation)?;
                println!("SIMULATION_USAGE {usage}");
            }
            _ => return Err("offline simulation only; usage: enroll DB ENROLLMENT_JSON | attempt DB EXPECTED_JSON SNAPSHOT_JSON VAULT SHA256 OPERATION DAY NOW [IGNORED_ADVERTISED_AMOUNT]".into())
        }
        Ok(())
    }
}
fn main() {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if let Err(error) = simulation::run() {
        eprintln!("SIMULATION_REFUSED {error}");
        std::process::exit(1);
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        eprintln!("offline simulation requires Linux or Android");
        std::process::exit(1);
    }
}
