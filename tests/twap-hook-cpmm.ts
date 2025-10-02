import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { TwapHookCpmm } from "../target/types/twap_hook_cpmm";
import { Keypair, PublicKey, SystemProgram, Transaction } from "@solana/web3.js";
import {
  createInitializeTransferHookInstruction,
  createInitializeMintInstruction,
  getMintLen,
  ExtensionType,
  TOKEN_2022_PROGRAM_ID,
  ASSOCIATED_TOKEN_PROGRAM_ID,
  getMint,
  getTransferHook,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  createSyncNativeInstruction,
  getAccount,
  NATIVE_MINT,
} from "@solana/spl-token";
import { expect } from "chai";
import { DEVNET_PROGRAM_ID, getCpmmPdaAmmConfigId, Raydium, DEV_API_URLS, TxVersion, ApiV3PoolInfoStandardItemCpmm, CpmmKeys, CpmmParsedRpcData, CurveCalculator, FeeOn } from "@raydium-io/raydium-sdk-v2";


describe("twap-hook-cpmm", () => {
  // Configure the client to use the local cluster.
  anchor.setProvider(anchor.AnchorProvider.env());
  const program = anchor.workspace.twapHookCpmm as Program<TwapHookCpmm>;

  const provider = anchor.getProvider();

  const mintKp = Keypair.generate();
  const mint = mintKp.publicKey;
  const payer = (provider.wallet as anchor.Wallet).payer;

  // Global variables to reuse between tests
  let poolId: PublicKey;
  let vaultA: PublicKey;
  let vaultB: PublicKey;
  let ataMyToken: any;
  let ataWsol: any;

  it("Should create SPL Token with Transfer HOOk", async () => {

    // ===== STEP 1: Configure the mint with Transfer Hook Extension =====

    // 1.2 Define the extensions to use from Token-2022
    const extensions = [ExtensionType.TransferHook];

    // 1.3 Calculate the required space for the mint and its extensions
    const space = getMintLen(extensions);

    // 1.4 Get rent-exempt price
    const lamports = await provider.connection.getMinimumBalanceForRentExemption(space);

    // 1.5 Create account for the mint (reserve space + rent)
    const ix0 = SystemProgram.createAccount({
      fromPubkey: payer.publicKey,
      newAccountPubkey: mintKp.publicKey,
      lamports: lamports,
      space: space,
      programId: TOKEN_2022_PROGRAM_ID
    })

    // 1.6 Initialize Transfer Hook extension (BEFORE the mint)
    const ix1 = createInitializeTransferHookInstruction(
      mint,                       // Token Mint account
      payer.publicKey,            // TransferHook authority account
      program.programId,          // TransferHook programId
      TOKEN_2022_PROGRAM_ID       // SPL Token program account
    );

    // 1.7 Initialize the mint (AFTER the extensions)
    const ix2 = createInitializeMintInstruction(
      mint,
      6,
      payer.publicKey,
      null,
      TOKEN_2022_PROGRAM_ID
    );

    // 1.8 Execute: Create account -> Initialize extensions -> Initialize mint
    const tx = new Transaction().add(ix0).add(ix1).add(ix2);
    const signature = await provider.sendAndConfirm(tx, [mintKp]);
    console.log("Mint creada y hook registrado. Signature: ", signature);

    // ===== STEP 2: Verifications on the Blockchain =====
    console.log("Verificando el estado de las cuentas en la blockchain...");

    // 2.1 Verify the state of the MINT account
    const mintInfo = await getMint(
      provider.connection,
      mint,
      "confirmed",
      TOKEN_2022_PROGRAM_ID
    );
    expect(mintInfo).to.not.be.null;                                                    // The mint must exist
    expect(mintInfo.mintAuthority.toBase58()).to.equal(payer.publicKey.toBase58());     // Must have the correct mint authority
    expect(mintInfo.decimals).to.equal(6);                                              // Must have the correct decimals

    // 2.2 Verify the Transfer Hook extension configuration
    const transferHookData = getTransferHook(mintInfo);
    expect(transferHookData).to.not.be.null;                                               // The extension must be initialized
    expect(transferHookData.programId.toBase58()).to.equal(program.programId.toBase58());  // The hook's programId must be our program's
  });

  it("Should create CPMM pool and initialize ExtraAccountMetaList + PriceRing", async () => {

    // ===== STEP 1: Create ATA accounts for WSOL and MyToken =====
    //const wsolMint = new PublicKey("So11111111111111111111111111111111111111112"); // Devnet WSOL

    const ataMyToken = await getOrCreateAssociatedTokenAccount(
      provider.connection,
      payer,
      mint,
      payer.publicKey,
      false,
      "confirmed",
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    console.log("ATA creada:", ataMyToken.address.toBase58());

    const ataWsol = await getOrCreateAssociatedTokenAccount(
      provider.connection,
      payer,
      NATIVE_MINT,        // "So11111111111111111111111111111111111111112"
      payer.publicKey,
      false,
      "confirmed"
    );

    // Send 0.6 SOL to the WSOL ATA
    const solTransferIx = SystemProgram.transfer({
      fromPubkey: payer.publicKey,
      toPubkey: ataWsol.address,
      lamports: 0.6 * 1e9, // 0.6 SOL
    });
    const syncIx = createSyncNativeInstruction(ataWsol.address)
    const fundTx = new Transaction().add(solTransferIx).add(syncIx)
    await provider.sendAndConfirm(fundTx, [payer])
    console.log("WSOL funded with 0.6 SOL");

    // ===== STEP 2: Mint tokens to the ATA =====
    const { blockhash, lastValidBlockHeight } = await provider.connection.getLatestBlockhash('confirmed');

    const signature = await mintTo(
      provider.connection,
      payer,
      mint,
      ataMyToken.address,
      payer,
      1_000 * 10 ** 6,
      [],
      undefined,
      TOKEN_2022_PROGRAM_ID
    );
    console.log("Transacción de minteo enviada:", signature);

    // Confirm the Tx
    await provider.connection.confirmTransaction({
      signature,
      blockhash,
      lastValidBlockHeight,
    }, 'confirmed');


    // ===== STEP 3: Create the Pool =====

    // 3.1 Load Raydium SDK
    const raydium = await Raydium.load({
      connection: provider.connection,
      owner: payer,
      disableLoadToken: false,
      blockhashCommitment: 'finalized',
      urlConfigs: {
        ...DEV_API_URLS,
        BASE_HOST: 'https://api-v3-devnet.raydium.io',
        OWNER_BASE_HOST: 'https://owner-v1-devnet.raydium.io',
        SWAP_HOST: 'https://transaction-v1-devnet.raydium.io',
        CPMM_LOCK: 'https://dynamic-ipfs-devnet.raydium.io/lock/cpmm/position',
      }
    })


    // 3.2 Get fee configs
    const [feeConfigs] = await Promise.all([raydium.api.getCpmmConfigs()]);

    // Fee config id (devnet pda)
    if (raydium.cluster === 'devnet') {
      feeConfigs.forEach((config) => {
        config.id = getCpmmPdaAmmConfigId(DEVNET_PROGRAM_ID.CREATE_CPMM_POOL_PROGRAM, config.index).publicKey.toBase58()
      })
    }

    // Get mint Info
    const mintA = await raydium.token.getTokenInfo(mint)
    const mintB = await raydium.token.getTokenInfo(NATIVE_MINT)

    const { execute, extInfo, transaction } = await raydium.cpmm.createPool({
      programId: DEVNET_PROGRAM_ID.CREATE_CPMM_POOL_PROGRAM,
      poolFeeAccount: DEVNET_PROGRAM_ID.CREATE_CPMM_POOL_FEE_ACC,
      mintA: mintA,
      mintB: mintB,
      mintAAmount: new anchor.BN(500 * 10 ** mintA.decimals),           // 500 tokens A (6 dec) => 500 000 000 atómicos,
      mintBAmount: new anchor.BN(0.6 * 10 ** mintB.decimals),           // 0.6 WSOL (9 dec) => 600 000 000 atómicos,
      startTime: new anchor.BN(0),
      feeConfig: feeConfigs[0],
      associatedOnly: false,
      ownerInfo: { useSOLBalance: true },
      txVersion: TxVersion.V0,
    })

    console.log("Pool Id: ", extInfo.address.poolId)
    const poolId = extInfo.address.poolId
    const vaultA = extInfo.address.vaultA
    const vaultB = extInfo.address.vaultB

    /**
     * THE SIMULATION FAILS BECAUSE CPMM DEVNET DONT SUPPORT TOKEN_2022 TRANSFER HOOK MINT EXTENSION
     * BUT I WILL CONTINUE WITH THE TEST LIKE IF IT DOES
     */
    const simResult = await provider.connection.simulateTransaction(transaction);
    console.log("Simulación => logs:", simResult.value.logs);
    console.log("Simulación => error:", simResult.value.err);

    // Send the Tx that creates the Pool
    const { txId } = await execute({ sendAndConfirm: true })
    console.log('pool created', {
      txId,
      poolKeys: Object.keys(extInfo.address).reduce(
        (acc, cur) => ({
          ...acc,
          [cur]: extInfo.address[cur as keyof typeof extInfo.address].toString(),
        }),
        {}
      ),
    })

    // Initialize extra accounts and Ring Buffer
    const initSig = await program.methods
      .initializeExtraAccountMetaList(
        poolId,
        vaultA,
        vaultB
      )
      .accounts({
        payer: payer.publicKey,
        mint: mint
      })
      .rpc()

    // Verify that ExtraAccountMetaList was created
    const [extraAccountMetaListPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("extra-account-metas"), mint.toBuffer()],
      program.programId
    );
    const extraAccountInfo = await provider.connection.getAccountInfo(extraAccountMetaListPda);
    expect(extraAccountInfo).to.not.be.null;
    expect(extraAccountInfo.owner.toBase58()).to.equal(program.programId.toBase58());

    // Verify that PriceRing was initialized correctly
    const [priceRingPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("price-ring"), mint.toBuffer()],
      program.programId
    );
    const priceRing = await program.account.priceRing.fetch(priceRingPda);
    expect(priceRing.head).to.equal(0);
    expect(priceRing.points.length).to.equal(512);

    console.log("ExtraAccountMetaList y PriceRing inicializadas:", initSig);
  })

  it("Should perform swaps, trigger the hook and update ring buffer", async () => {

    // Load Raydium SDK 
    const raydium = await Raydium.load({
      connection: provider.connection,
      owner: payer,
      disableLoadToken: false,
      blockhashCommitment: 'finalized',
      urlConfigs: {
        ...DEV_API_URLS,
        BASE_HOST: 'https://api-v3-devnet.raydium.io',
        OWNER_BASE_HOST: 'https://owner-v1-devnet.raydium.io',
        SWAP_HOST: 'https://transaction-v1-devnet.raydium.io',
        CPMM_LOCK: 'https://dynamic-ipfs-devnet.raydium.io/lock/cpmm/position',
      }
    });

    // Get pool information
    const data = await raydium.cpmm.getPoolInfoFromRpc(poolId.toBase58());
    const poolInfo: ApiV3PoolInfoStandardItemCpmm = data.poolInfo;
    const poolKeys: CpmmKeys | undefined = data.poolKeys;
    const rpcData: CpmmParsedRpcData = data.rpcData;

    // Read ring buffer BEFORE the swap
    const [priceRingPda] = PublicKey.findProgramAddressSync(
      [Buffer.from("price-ring"), mint.toBuffer()],
      program.programId
    );
    const priceRingBefore = await program.account.priceRing.fetch(priceRingPda);
    const headBefore = priceRingBefore.head;

    /**
      * Execute a SWAP
    */

    // 1. I have 0.01 SOL and I want MyToken
    const inputAmount = new anchor.BN(0.01 * 10 ** 9); // 0.01 SOL
    const inputMint = NATIVE_MINT.toBase58();
    // 2. Determine swap direction
    const baseIn = inputMint === poolInfo.mintA.address;  // false (because WSOL is mintB)

    // 3. Calculate how much I WILL RECEIVE using pool mathematics
    const swapResult = CurveCalculator.swapBaseInput(
      inputAmount,                                            // We give: 0.01 SOL
      baseIn ? rpcData.baseReserve : rpcData.quoteReserve,    // Pool has: 0.6 SOL (because baseIn=false, we use quote)
      baseIn ? rpcData.quoteReserve : rpcData.baseReserve,    // Pool has: 500 MyTokens
      rpcData.configInfo!.tradeFeeRate,
      rpcData.configInfo!.creatorFeeRate,
      rpcData.configInfo!.protocolFeeRate,
      rpcData.configInfo!.fundFeeRate,
      rpcData.feeOn === FeeOn.BothToken || rpcData.feeOn === FeeOn.OnlyTokenB
    );
    // swapResult.outputAmount = ~8.05 MyTokens

    // 4. Execute the swap on the blockchain with these calculations
    const { execute } = await raydium.cpmm.swap({
      poolInfo,
      poolKeys,
      inputAmount,               // We give 0.01 SOL
      swapResult,                // We expect to receive ~8.05 MyTokens
      slippage: 0.01,            // Direction: quote => base (WSOL => MyToken)
      baseIn,
      txVersion: TxVersion.V0,
    });

    await execute({ sendAndConfirm: true });

    // Read ring buffer AFTER the swap
    const priceRingAfter = await program.account.priceRing.fetch(priceRingPda);
    const headAfter = priceRingAfter.head;

    // Verify that the head advanced exactly 1 position
    expect(headAfter).to.equal(headBefore + 1);

    // Verify that the price was saved in the correct position
    const pricePoint = priceRingAfter.points[headBefore];
    expect(pricePoint.slot.toNumber()).to.be.greaterThan(0);
    expect(pricePoint.price.toNumber()).to.be.greaterThan(0);
  })
});
