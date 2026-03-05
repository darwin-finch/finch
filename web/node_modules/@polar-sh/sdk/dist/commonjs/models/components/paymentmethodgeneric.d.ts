import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type PaymentMethodGeneric = {
    id: string;
    type: string;
    createdAt: Date;
    default: boolean;
};
/** @internal */
export declare const PaymentMethodGeneric$inboundSchema: z.ZodType<PaymentMethodGeneric, z.ZodTypeDef, unknown>;
/** @internal */
export type PaymentMethodGeneric$Outbound = {
    id: string;
    type: string;
    created_at: string;
    default: boolean;
};
/** @internal */
export declare const PaymentMethodGeneric$outboundSchema: z.ZodType<PaymentMethodGeneric$Outbound, z.ZodTypeDef, PaymentMethodGeneric>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace PaymentMethodGeneric$ {
    /** @deprecated use `PaymentMethodGeneric$inboundSchema` instead. */
    const inboundSchema: z.ZodType<PaymentMethodGeneric, z.ZodTypeDef, unknown>;
    /** @deprecated use `PaymentMethodGeneric$outboundSchema` instead. */
    const outboundSchema: z.ZodType<PaymentMethodGeneric$Outbound, z.ZodTypeDef, PaymentMethodGeneric>;
    /** @deprecated use `PaymentMethodGeneric$Outbound` instead. */
    type Outbound = PaymentMethodGeneric$Outbound;
}
export declare function paymentMethodGenericToJSON(paymentMethodGeneric: PaymentMethodGeneric): string;
export declare function paymentMethodGenericFromJSON(jsonString: string): SafeParseResult<PaymentMethodGeneric, SDKValidationError>;
//# sourceMappingURL=paymentmethodgeneric.d.ts.map