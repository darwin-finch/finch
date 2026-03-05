import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type MeterQuantity = {
    /**
     * The timestamp for the current period.
     */
    timestamp: Date;
    /**
     * The quantity for the current period.
     */
    quantity: number;
};
/** @internal */
export declare const MeterQuantity$inboundSchema: z.ZodType<MeterQuantity, z.ZodTypeDef, unknown>;
/** @internal */
export type MeterQuantity$Outbound = {
    timestamp: string;
    quantity: number;
};
/** @internal */
export declare const MeterQuantity$outboundSchema: z.ZodType<MeterQuantity$Outbound, z.ZodTypeDef, MeterQuantity>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace MeterQuantity$ {
    /** @deprecated use `MeterQuantity$inboundSchema` instead. */
    const inboundSchema: z.ZodType<MeterQuantity, z.ZodTypeDef, unknown>;
    /** @deprecated use `MeterQuantity$outboundSchema` instead. */
    const outboundSchema: z.ZodType<MeterQuantity$Outbound, z.ZodTypeDef, MeterQuantity>;
    /** @deprecated use `MeterQuantity$Outbound` instead. */
    type Outbound = MeterQuantity$Outbound;
}
export declare function meterQuantityToJSON(meterQuantity: MeterQuantity): string;
export declare function meterQuantityFromJSON(jsonString: string): SafeParseResult<MeterQuantity, SDKValidationError>;
//# sourceMappingURL=meterquantity.d.ts.map