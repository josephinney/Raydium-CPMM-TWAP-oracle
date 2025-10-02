import { AnchorProvider, Wallet } from "@coral-xyz/anchor";
import { Connection, Keypair, SystemProgram, PublicKey, Transaction } from "@solana/web3.js";
import * as fs from 'fs';
import os from 'os';

/**
 * Esta función configura el 'Provider' de Anchor manualmente, leyendo la configuración
 * directamente del sistema. Es la forma más robusta de hacerlo en un script independiente.
 */
function getProvider() {
    // 1. Conectarse al cluster de Devnet.
    const connection = new Connection("https://api.devnet.solana.com", "confirmed");

    // 2. Cargar el keypair de la billetera desde la ruta por defecto de Solana.
    // Esto lee el archivo id.json que configuraste.
    const keypairPath = os.homedir() + "/.config/solana/id.json";
    const secretKey = JSON.parse(fs.readFileSync(keypairPath, 'utf-8'));
    const payerKeypair = Keypair.fromSecretKey(new Uint8Array(secretKey));
    
    // 3. Crear un objeto Wallet de Anchor.
    const wallet = new Wallet(payerKeypair);

    // 4. Crear el Anchor Provider, que combina la conexión y la billetera.
    const provider = new AnchorProvider(connection, wallet, {
        preflightCommitment: "confirmed",
        commitment: "confirmed",
    });

    return { provider, payerKeypair };
}

async function createFeeAccount() {
  console.log("Iniciando la creación de la cuenta de fees...");

  // Usamos nuestra función de configuración explícita.
  const { provider, payerKeypair } = getProvider();
  const connection = provider.connection;

  // El Program ID del programa forkeado
  const MY_CPMM_PROGRAM = new PublicKey("AXz2trx51zUQ35W7gonmLiQUVPSdrM82VG1KJNWdym4x");

  // Generamos un nuevo keypair para la cuenta de fees que vamos a crear.
  const feeAccountKp = Keypair.generate();
  console.log(`La Public Key de la nueva Pool Fee Account será: ${feeAccountKp.publicKey.toBase58()}`);

  
  const ACCOUNT_SIZE = 100; 

  const lamports = await connection.getMinimumBalanceForRentExemption(ACCOUNT_SIZE);

  const tx = new Transaction().add(
    SystemProgram.createAccount({
      fromPubkey: payerKeypair.publicKey, 
      newAccountPubkey: feeAccountKp.publicKey, 
      lamports,
      space: ACCOUNT_SIZE,
      programId: MY_CPMM_PROGRAM, 
    })
  );

  const signature = await provider.sendAndConfirm(tx, [feeAccountKp]);

  console.log(`\n¡Éxito! Cuenta de fees creada.`);
  console.log(`-> Signature: ${signature}`);
  console.log(`-> Dirección de la cuenta: ${feeAccountKp.publicKey.toBase58()}`);
  console.log(`[${feeAccountKp.secretKey.toString()}]`);
}

// Ejecutamos la función.
createFeeAccount().catch(error => {
  console.error("Ocurrió un error al crear la cuenta de fees:", error);
  process.exit(1);
});



const MY_FORKED_FEE_ACCOUNT = new PublicKey("G9o3sZNBvWvF7Q1TDeEyggK8HsywQPUSzEd4yxJRyHb1");
const MY_CPMM_PROGRAM = new PublicKey("AXz2trx51zUQ35W7gonmLiQUVPSdrM82VG1KJNWdym4x"); // Forkeado
