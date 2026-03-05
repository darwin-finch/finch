import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type PaymentMethodCardData = {
    brand: string;
    last4: string;
    expMonth: number;
    expYear: number;
    wallet?: string | null | undefined;
};
/** @internal */
export declare const PaymentMethodCardData$inboundSchema: z.ZodType<PaymentMethodCardData, z.ZodTypeDef, unknown>;
/** @internal */
export type PaymentMethodCardData$Outbound = {
    brand: string;
    last4: string;
    exp_month: number;
    exp_year: number;
    wallet?: string | null | undefined;
};
/** @internal */
export declare const PaymentMethodCardData$outboundSchema: z.ZodType<PaymentMethodCardData$Outbound, z.ZodTypeDef, PaymentMethodCardData>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace PaymentMethodCardData$ {
    /** @deprecated use `PaymentMethodCardData$inboundSchema` instead. */
    const inboundSchema: z.ZodType<PaymentMethodCardData, z.ZodTypeDef, unknown>;
    /** @deprecated use `PaymentMethodCardData$outboundSchema` instead. */
    const outboundSchema: z.ZodType<PaymentMethodCardData$Outbound, z.ZodTypeDef, PaymentMethodCardData>;
    /** @deprecated use `PaymentMethodCardData$Outbound` instead. */
    type Outbound = PaymentMethodCardData$Outbound;
}
export declare function paymentMethodCardDataToJSON(paymentMethodCardData: PaymentMethodCardData): string;
export declare function paymentMethodCardDataFromJSON(jsonString: string): SafeParseResult<PaymentMethodCardData, SDKValidationError>;
//# sourceMappingURL=paymentmethodcarddata.d.ts.map