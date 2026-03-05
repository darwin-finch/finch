import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { PaymentMethodCardData, PaymentMethodCardData$Outbound } from "./paymentmethodcarddata.js";
export type PaymentMethodCard = {
    id: string;
    type?: "card" | undefined;
    createdAt: Date;
    default: boolean;
    card: PaymentMethodCardData;
};
/** @internal */
export declare const PaymentMethodCard$inboundSchema: z.ZodType<PaymentMethodCard, z.ZodTypeDef, unknown>;
/** @internal */
export type PaymentMethodCard$Outbound = {
    id: string;
    type: "card";
    created_at: string;
    default: boolean;
    card: PaymentMethodCardData$Outbound;
};
/** @internal */
export declare const PaymentMethodCard$outboundSchema: z.ZodType<PaymentMethodCard$Outbound, z.ZodTypeDef, PaymentMethodCard>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace PaymentMethodCard$ {
    /** @deprecated use `PaymentMethodCard$inboundSchema` instead. */
    const inboundSchema: z.ZodType<PaymentMethodCard, z.ZodTypeDef, unknown>;
    /** @deprecated use `PaymentMethodCard$outboundSchema` instead. */
    const outboundSchema: z.ZodType<PaymentMethodCard$Outbound, z.ZodTypeDef, PaymentMethodCard>;
    /** @deprecated use `PaymentMethodCard$Outbound` instead. */
    type Outbound = PaymentMethodCard$Outbound;
}
export declare function paymentMethodCardToJSON(paymentMethodCard: PaymentMethodCard): string;
export declare function paymentMethodCardFromJSON(jsonString: string): SafeParseResult<PaymentMethodCard, SDKValidationError>;
//# sourceMappingURL=paymentmethodcard.d.ts.map